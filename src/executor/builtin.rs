use std::path::{Path, PathBuf};

use crate::catalog::{RecipeArchiveFormat, RecipeBuildSystem};

use super::operation::{CommandOperation, ExecutionTarget, OperationSpec};
use super::plan::BuiltinRecipePlan;

pub fn plan_builtin_operations(
    item_id: &str,
    recipe: &BuiltinRecipePlan,
    target: ExecutionTarget,
) -> Vec<OperationSpec> {
    match recipe {
        BuiltinRecipePlan::DirectBinaryInstall {
            url,
            destination,
            binary_name,
            ..
        } => plan_direct_binary(item_id, url, destination, binary_name, target),
        BuiltinRecipePlan::ArchiveInstall {
            url,
            destination_dir,
            format,
            binary_name,
            member_path,
            strip_components,
            ..
        } => plan_archive(
            item_id,
            url,
            destination_dir,
            format,
            binary_name,
            member_path.as_deref(),
            *strip_components,
            target,
        ),
        BuiltinRecipePlan::SourceBuildInstall {
            source_url,
            revision,
            build_system,
            working_subdir,
            install_prefix,
        } => plan_source_build(
            item_id,
            source_url,
            revision.as_deref(),
            *build_system,
            working_subdir.as_deref(),
            install_prefix.as_path(),
            target,
        ),
    }
}

fn plan_direct_binary(
    item_id: &str,
    url: &str,
    destination: &Path,
    binary_name: &str,
    target: ExecutionTarget,
) -> Vec<OperationSpec> {
    let staging_root = staging_root(item_id, "direct-binary");
    let staging_file = staging_root.join(binary_name);
    vec![
        mkdir(
            destination.parent().unwrap_or_else(|| Path::new("/")),
            target,
        ),
        mkdir(&staging_root, target),
        command("curl", ["-fsSL", url, "-o"], [&staging_file], target),
        command("chmod", ["755"], [&staging_file], target),
        command(
            "install",
            ["-m", "755"],
            [&staging_file, destination],
            target,
        ),
    ]
}

fn plan_archive(
    item_id: &str,
    url: &str,
    destination_dir: &Path,
    format: &RecipeArchiveFormat,
    binary_name: &str,
    member_path: Option<&Path>,
    strip_components: u32,
    target: ExecutionTarget,
) -> Vec<OperationSpec> {
    let staging_root = staging_root(item_id, "archive");
    let archive_path = staging_root.join(archive_filename(url, format));
    let destination_binary = destination_dir.join(binary_name);
    let mut operations = vec![
        mkdir(destination_dir, target),
        mkdir(&staging_root, target),
        command("curl", ["-fsSL", url, "-o"], [&archive_path], target),
    ];

    operations.push(match format {
        RecipeArchiveFormat::TarGz => tar_extract(
            "-xzf",
            &archive_path,
            destination_dir,
            member_path,
            strip_components,
            target,
        ),
        RecipeArchiveFormat::TarXz => tar_extract(
            "-xJf",
            &archive_path,
            destination_dir,
            member_path,
            strip_components,
            target,
        ),
        RecipeArchiveFormat::TarBz2 => tar_extract(
            "-xjf",
            &archive_path,
            destination_dir,
            member_path,
            strip_components,
            target,
        ),
        RecipeArchiveFormat::Zip => {
            unzip_extract(&archive_path, destination_dir, member_path, target)
        }
    });
    operations.push(command("chmod", ["755"], [&destination_binary], target));
    operations
}

fn plan_source_build(
    item_id: &str,
    source_url: &str,
    revision: Option<&str>,
    build_system: RecipeBuildSystem,
    working_subdir: Option<&Path>,
    install_prefix: &Path,
    target: ExecutionTarget,
) -> Vec<OperationSpec> {
    let workspace_root = staging_root(item_id, "source-build");
    let source_dir = workspace_root.join("source");
    let build_dir = source_dir.join("build");
    let working_dir =
        working_subdir.map_or_else(|| source_dir.clone(), |path| source_dir.join(path));
    let mut operations = vec![
        mkdir(
            workspace_root.parent().unwrap_or_else(|| Path::new("/tmp")),
            target,
        ),
        command(
            "git",
            ["clone", "--depth", "1", source_url],
            [&source_dir],
            target,
        ),
    ];

    if let Some(revision) = revision {
        operations.push(
            CommandOperation::new("git")
                .with_args(["checkout", "--detach", revision])
                .with_cwd(&source_dir)
                .with_target(target)
                .into(),
        );
    }

    match build_system {
        RecipeBuildSystem::Autotools => {
            operations.push(
                CommandOperation::new("./configure")
                    .with_args([format!("--prefix={}", install_prefix.display())])
                    .with_cwd(&working_dir)
                    .with_target(target)
                    .into(),
            );
            operations.push(make(&working_dir, [], target));
            operations.push(make(&working_dir, ["install".to_string()], target));
        }
        RecipeBuildSystem::Cmake => {
            operations.push(mkdir(&build_dir, target));
            operations.push(
                CommandOperation::new("cmake")
                    .with_args([
                        "-S".to_string(),
                        ".".to_string(),
                        "-B".to_string(),
                        build_dir.to_string_lossy().into_owned(),
                        format!("-DCMAKE_INSTALL_PREFIX={}", install_prefix.display()),
                    ])
                    .with_cwd(&working_dir)
                    .with_target(target)
                    .into(),
            );
            operations.push(
                CommandOperation::new("cmake")
                    .with_args([
                        "--build".to_string(),
                        build_dir.to_string_lossy().into_owned(),
                    ])
                    .with_target(target)
                    .into(),
            );
            operations.push(
                CommandOperation::new("cmake")
                    .with_args([
                        "--install".to_string(),
                        build_dir.to_string_lossy().into_owned(),
                    ])
                    .with_target(target)
                    .into(),
            );
        }
        RecipeBuildSystem::Cargo => {
            operations.push(
                CommandOperation::new("cargo")
                    .with_args([
                        "install".to_string(),
                        "--path".to_string(),
                        ".".to_string(),
                        "--root".to_string(),
                        install_prefix.to_string_lossy().into_owned(),
                    ])
                    .with_cwd(&working_dir)
                    .with_target(target)
                    .into(),
            );
        }
        RecipeBuildSystem::Go => {
            operations.push(
                CommandOperation::new("go")
                    .with_args(["install", "."])
                    .with_env([(
                        "GOBIN",
                        install_prefix.join("bin").to_string_lossy().into_owned(),
                    )])
                    .with_cwd(&working_dir)
                    .with_target(target)
                    .into(),
            );
        }
        RecipeBuildSystem::Python => {
            operations.push(
                CommandOperation::new("python3")
                    .with_args([
                        "-m".to_string(),
                        "pip".to_string(),
                        "install".to_string(),
                        ".".to_string(),
                        "--prefix".to_string(),
                        install_prefix.to_string_lossy().into_owned(),
                    ])
                    .with_cwd(&working_dir)
                    .with_target(target)
                    .into(),
            );
        }
        RecipeBuildSystem::Make => {
            operations.push(make(&working_dir, [], target));
            let mut install_args = vec!["install".to_string()];
            install_args.push(format!("PREFIX={}", install_prefix.display()));
            operations.push(make(&working_dir, install_args, target));
        }
    }

    operations
}

fn tar_extract(
    mode: &str,
    archive_path: &Path,
    destination_dir: &Path,
    member_path: Option<&Path>,
    strip_components: u32,
    target: ExecutionTarget,
) -> OperationSpec {
    let mut args = vec![
        mode.to_string(),
        archive_path.to_string_lossy().into_owned(),
        "-C".to_string(),
        destination_dir.to_string_lossy().into_owned(),
    ];
    if strip_components > 0 {
        args.push("--strip-components".to_string());
        args.push(strip_components.to_string());
    }
    if let Some(member_path) = member_path {
        args.push(member_path.to_string_lossy().into_owned());
    }
    CommandOperation::new("tar")
        .with_args(args)
        .with_target(target)
        .into()
}

fn unzip_extract(
    archive_path: &Path,
    destination_dir: &Path,
    member_path: Option<&Path>,
    target: ExecutionTarget,
) -> OperationSpec {
    let mut args = vec!["-o".to_string()];
    if member_path.is_some() {
        args.push("-j".to_string());
    }
    args.push(archive_path.to_string_lossy().into_owned());
    if let Some(member_path) = member_path {
        args.push(member_path.to_string_lossy().into_owned());
    }
    args.push("-d".to_string());
    args.push(destination_dir.to_string_lossy().into_owned());
    CommandOperation::new("unzip")
        .with_args(args)
        .with_target(target)
        .into()
}

fn make<I>(working_dir: &Path, args: I, target: ExecutionTarget) -> OperationSpec
where
    I: IntoIterator<Item = String>,
{
    CommandOperation::new("make")
        .with_args(args)
        .with_cwd(working_dir)
        .with_target(target)
        .into()
}

fn mkdir(path: &Path, target: ExecutionTarget) -> OperationSpec {
    command("mkdir", ["-p"], [path], target)
}

fn command<const A: usize, const P: usize>(
    program: &str,
    base_args: [&str; A],
    path_args: [&Path; P],
    target: ExecutionTarget,
) -> OperationSpec {
    let mut args = base_args
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
    args.extend(
        path_args
            .iter()
            .map(|path| path.to_string_lossy().into_owned()),
    );
    CommandOperation::new(program)
        .with_args(args)
        .with_target(target)
        .into()
}

fn archive_filename(url: &str, format: &RecipeArchiveFormat) -> String {
    Path::new(url)
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| match format {
            RecipeArchiveFormat::TarGz => "archive.tar.gz".to_string(),
            RecipeArchiveFormat::TarXz => "archive.tar.xz".to_string(),
            RecipeArchiveFormat::TarBz2 => "archive.tar.bz2".to_string(),
            RecipeArchiveFormat::Zip => "archive.zip".to_string(),
        })
}

fn staging_root(item_id: &str, family: &str) -> PathBuf {
    PathBuf::from("/tmp")
        .join("envira")
        .join(family)
        .join(item_id)
}

impl From<CommandOperation> for OperationSpec {
    fn from(value: CommandOperation) -> Self {
        Self::Command(value)
    }
}
