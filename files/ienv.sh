# ~/.local/share/infisical/ienv.sh
# POSIX-compatible Infisical env loader for sh/bash/zsh.
#
# Required environment variables:
#   INFISICAL_CLIENT_ID
#   INFISICAL_CLIENT_SECRET
#   INFISICAL_PROJECT_ID
#
# Optional environment variables:
#   INFISICAL_ENV=dev
#   INFISICAL_SECRET_PATH=/
#   INFISICAL_ENV_TTL=86400
#   INFISICAL_CACHE_DIR=$HOME/.cache/infisical
#
# Usage after sourcing:
#   ienv
#   ienv -f
#   ienv --quiet
#   ienv -f --quiet

# Skip ienv completely if Infisical credentials are not configured.
if [ -z "${INFISICAL_CLIENT_ID:-}" ] && \
   [ -z "${INFISICAL_CLIENT_SECRET:-}" ] && \
   [ -z "${INFISICAL_PROJECT_ID:-}" ]; then
  return 0 2>/dev/null || exit 0
fi

_ienv_info() {
  [ "${_ienv_quiet:-0}" = "1" ] || printf '[ienv] %s\n' "$*" >&2
}

_ienv_error() {
  printf '[ienv] ERROR: %s\n' "$*" >&2
}

_ienv_mtime() {
  stat -c %Y "$1" 2>/dev/null || stat -f %m "$1" 2>/dev/null
}

_ienv_age() {
  _ienv_file="$1"
  [ -s "$_ienv_file" ] || return 1

  _ienv_file_mtime="$(_ienv_mtime "$_ienv_file")" || return 1
  _ienv_now="$(date +%s)"

  echo $(( _ienv_now - _ienv_file_mtime ))
}

_ienv_is_fresh() {
  _ienv_file="$1"
  _ienv_ttl="$2"

  [ -s "$_ienv_file" ] || return 1

  case "$_ienv_ttl" in
    ''|*[!0-9]*) return 1 ;;
  esac

  [ "$_ienv_ttl" -gt 0 ] || return 1

  _ienv_file_age="$(_ienv_age "$_ienv_file")" || return 1
  [ "$_ienv_file_age" -lt "$_ienv_ttl" ]
}

_ienv_require_config() {
  [ -n "${INFISICAL_CLIENT_ID:-}" ] || {
    _ienv_error "missing INFISICAL_CLIENT_ID"
    return 1
  }

  [ -n "${INFISICAL_CLIENT_SECRET:-}" ] || {
    _ienv_error "missing INFISICAL_CLIENT_SECRET"
    return 1
  }

  [ -n "${INFISICAL_PROJECT_ID:-}" ] || {
    _ienv_error "missing INFISICAL_PROJECT_ID"
    return 1
  }

  command -v infisical >/dev/null 2>&1 || {
    _ienv_error "missing infisical CLI"
    return 1
  }
}

_ienv_login() {
  _ienv_cache_dir="$1"
  _ienv_token_cache="$2"
  _ienv_errfile="$3"

  _ienv_tmp_token="$(mktemp "$_ienv_cache_dir/token.XXXXXX")" || return 1

  INFISICAL_DISABLE_UPDATE_CHECK=true \
  infisical login \
    --method=universal-auth \
    --client-id="$INFISICAL_CLIENT_ID" \
    --client-secret="$INFISICAL_CLIENT_SECRET" \
    --plain > "$_ienv_tmp_token" 2>"$_ienv_errfile" || {
      rm -f "$_ienv_tmp_token"
      return 1
    }

  [ -s "$_ienv_tmp_token" ] || {
    rm -f "$_ienv_tmp_token"
    echo "infisical login returned an empty token" > "$_ienv_errfile"
    return 1
  }

  chmod 600 "$_ienv_tmp_token" 2>/dev/null
  mv "$_ienv_tmp_token" "$_ienv_token_cache"
}

_ienv_remote_export() {
  _ienv_token="$1"
  _ienv_out="$2"
  _ienv_errfile="$3"

  INFISICAL_TOKEN="$_ienv_token" \
  INFISICAL_DISABLE_UPDATE_CHECK=true \
  infisical export \
    --projectId="$INFISICAL_PROJECT_ID" \
    --env="${INFISICAL_ENV:-dev}" \
    --path="${INFISICAL_SECRET_PATH:-/}" \
    --format="dotenv-export" > "$_ienv_out" 2>"$_ienv_errfile"
}

_ienv_print_last_error() {
  _ienv_errfile="$1"

  if [ -s "$_ienv_errfile" ]; then
    cat "$_ienv_errfile" >&2
  fi
}

ienv() {
  _ienv_force=0
  _ienv_quiet=0

  while [ "$#" -gt 0 ]; do
    case "$1" in
      -f|--force)
        _ienv_force=1
        ;;
      -q|--quiet)
        _ienv_quiet=1
        ;;
      -h|--help)
        cat <<'IENVEHELP'
Usage:
  ienv             Load Infisical env vars, using cache when fresh
  ienv -f          Force refresh from Infisical
  ienv --quiet     Suppress success messages
  ienv -f --quiet  Force refresh quietly

Required:
  INFISICAL_CLIENT_ID
  INFISICAL_CLIENT_SECRET
  INFISICAL_PROJECT_ID

Optional:
  INFISICAL_ENV          default: dev
  INFISICAL_SECRET_PATH  default: /
  INFISICAL_ENV_TTL      default: 86400
  INFISICAL_CACHE_DIR    default: ~/.cache/infisical
IENVEHELP
        return 0
        ;;
      *)
        _ienv_error "unknown argument: $1"
        echo "Usage: ienv [-f|--force] [-q|--quiet]" >&2
        return 2
        ;;
    esac
    shift
  done

  _ienv_cache_dir="${INFISICAL_CACHE_DIR:-$HOME/.cache/infisical}"
  _ienv_token_cache="$_ienv_cache_dir/token"
  _ienv_env_cache="$_ienv_cache_dir/env"
  _ienv_env_ttl="${INFISICAL_ENV_TTL:-86400}"

  mkdir -p "$_ienv_cache_dir" || return 1
  chmod 700 "$_ienv_cache_dir" 2>/dev/null

  # Fast path: source fresh env cache.
  if [ "$_ienv_force" -eq 0 ] && _ienv_is_fresh "$_ienv_env_cache" "$_ienv_env_ttl"; then
    . "$_ienv_env_cache" || {
      _ienv_error "failed to source cache: $_ienv_env_cache"
      return 1
    }

    _ienv_info "loaded from cache: $_ienv_env_cache"
    return 0
  fi

  _ienv_require_config || return 1

  _ienv_errfile="$(mktemp "$_ienv_cache_dir/error.XXXXXX")" || return 1
  _ienv_tmp_env="$(mktemp "$_ienv_cache_dir/env.XXXXXX")" || {
    rm -f "$_ienv_errfile"
    return 1
  }

  # Use cached token first. No TTL. If it is expired/revoked, export will fail and trigger re-login.
  if [ -s "$_ienv_token_cache" ]; then
    _ienv_token="$(cat "$_ienv_token_cache")"
  else
    if ! _ienv_login "$_ienv_cache_dir" "$_ienv_token_cache" "$_ienv_errfile"; then
      rm -f "$_ienv_tmp_env"

      if [ "$_ienv_force" -eq 0 ] && [ -s "$_ienv_env_cache" ]; then
        . "$_ienv_env_cache" || {
          rm -f "$_ienv_errfile"
          _ienv_error "login failed and stale cache could not be sourced"
          return 1
        }

        rm -f "$_ienv_errfile"
        _ienv_info "login failed; loaded stale cache: $_ienv_env_cache"
        return 0
      fi

      _ienv_print_last_error "$_ienv_errfile"
      rm -f "$_ienv_errfile"
      _ienv_error "login failed"
      return 1
    fi

    _ienv_token="$(cat "$_ienv_token_cache")"
  fi

  # First export attempt.
  if ! _ienv_remote_export "$_ienv_token" "$_ienv_tmp_env" "$_ienv_errfile"; then
    rm -f "$_ienv_token_cache"

    # Token may be expired/revoked. Re-login and retry once.
    : > "$_ienv_errfile"

    if ! _ienv_login "$_ienv_cache_dir" "$_ienv_token_cache" "$_ienv_errfile"; then
      rm -f "$_ienv_tmp_env"

      if [ "$_ienv_force" -eq 0 ] && [ -s "$_ienv_env_cache" ]; then
        . "$_ienv_env_cache" || {
          rm -f "$_ienv_errfile"
          _ienv_error "re-login failed and stale cache could not be sourced"
          return 1
        }

        rm -f "$_ienv_errfile"
        _ienv_info "re-login failed; loaded stale cache: $_ienv_env_cache"
        return 0
      fi

      _ienv_print_last_error "$_ienv_errfile"
      rm -f "$_ienv_errfile"
      _ienv_error "re-login failed"
      return 1
    fi

    _ienv_token="$(cat "$_ienv_token_cache")"
    : > "$_ienv_errfile"

    if ! _ienv_remote_export "$_ienv_token" "$_ienv_tmp_env" "$_ienv_errfile"; then
      rm -f "$_ienv_tmp_env"

      if [ "$_ienv_force" -eq 0 ] && [ -s "$_ienv_env_cache" ]; then
        . "$_ienv_env_cache" || {
          rm -f "$_ienv_errfile"
          _ienv_error "export failed and stale cache could not be sourced"
          return 1
        }

        rm -f "$_ienv_errfile"
        _ienv_info "export failed; loaded stale cache: $_ienv_env_cache"
        return 0
      fi

      _ienv_print_last_error "$_ienv_errfile"
      rm -f "$_ienv_errfile"
      _ienv_error "export failed"
      return 1
    fi
  fi

  rm -f "$_ienv_errfile"

  [ -s "$_ienv_tmp_env" ] || {
    rm -f "$_ienv_tmp_env"
    _ienv_error "export returned empty env"
    return 1
  }

  chmod 600 "$_ienv_tmp_env" 2>/dev/null
  mv "$_ienv_tmp_env" "$_ienv_env_cache"

  . "$_ienv_env_cache" || {
    _ienv_error "failed to source refreshed env cache"
    return 1
  }

  _ienv_info "refreshed from Infisical: $_ienv_env_cache"
  return 0
}