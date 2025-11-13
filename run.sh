#!/bin/bash
 
# install dev tools
mkdir -p ~/.local/bin
# if fedora or centos
if [[ -f /etc/redhat-release ]]; then

    # if fedora
    if cat /etc/redhat-release | grep -qiE "Fedora"; then
        sudo dnf install -y python3 pipx gedit vim git git-lfs curl wget zsh gcc make perl screen tmux ncdu xsel unzip bat screenfetch neofetch fastfetch mosh iperf3 nmap btop
    # if centos
    else
        sudo yum install -y python3 dnf gedit vim git git-lfs curl wget zsh gcc make perl build-essential screen tmux ncdu xsel unzip iperf3 nmap
        # install pipx
        python3 -m pip install --user pipx
        python3 -m pipx ensurepath

        # install bat
        wget -O bat.zip https://github.com/sharkdp/bat/releases/download/v0.7.1/bat-v0.7.1-x86_64-unknown-linux-musl.tar.gz
        tar -xvzf bat.zip -C ~/.local/bin
        cd ~/.local/bin && mv bat-v0.7.1-x86_64-unknown-linux-musl/bat . && rm -r bat-v0.7.1-x86_64-unknown-linux-musl
        cd ~ && rm bat.zip
        
        # install neofetch
        sudo curl -o /etc/yum.repos.d/konimex-neofetch-epel-7.repo https://copr.fedorainfracloud.org/coprs/konimex/neofetch/repo/epel-7/konimex-neofetch-epel-7.repo
        sudo yum install -y neofetch

        # install btop
        wget https://github.com/aristocratos/btop/releases/download/v1.4.0/btop-x86_64-linux-musl.tbz -O btop.tbz
        tar -xvf btop.tbz
        cd btop && ./install.sh
        cd ..
        rm -r btop
        rm btop.tbz
    fi

    # install ctop
    sudo wget https://github.com/bcicen/ctop/releases/download/v0.7.7/ctop-0.7.7-linux-amd64 -O /usr/local/bin/ctop
    sudo chmod +x /usr/local/bin/ctop

    # install vnc
    sudo dnf install -y tigervnc-server

    # install gitkraken
    wget https://release.gitkraken.com/linux/gitkraken-amd64.rpm
    sudo dnf install ./gitkraken-amd64.rpm
    rm gitkraken-amd64.rpm

# if ubuntu or mint
elif cat /etc/issue | grep -qiE "Mint|Ubuntu|Pop\!_OS"; then
    export DEBIAN_FRONTEND=noninteractive
    # update and install
    sudo apt update
    sudo apt install -y iputils-ping net-tools python3-venv apt-utils make openssh-server gedit vim git git-lfs curl wget zsh gcc make perl build-essential libfuse2 python3-pip screen tmux ncdu bat pipx xsel screenfetch neofetch p7zip-full unzip mosh nmap

    # create a symlink for batcat to bat
    ln -s /usr/bin/batcat ~/.local/bin/bat

    # install ctop
    sudo wget https://github.com/bcicen/ctop/releases/download/v0.7.7/ctop-0.7.7-linux-amd64 -O /usr/local/bin/ctop
    sudo chmod +x /usr/local/bin/ctop

    # install vnc
    sudo apt install -y tigervnc-standalone-server tigervnc-common tigervnc-xorg-extension

    # install gitkraken
    wget https://release.gitkraken.com/linux/gitkraken-amd64.deb
    sudo apt install -y ./gitkraken-amd64.deb
    rm gitkraken-amd64.deb

    # install fastfetch
    sudo add-apt-repository -y ppa:zhangsongcui3371/fastfetch
    sudo apt update
    sudo apt install -y fastfetch

    # install btop
    wget https://github.com/aristocratos/btop/releases/download/v1.4.0/btop-x86_64-linux-musl.tbz -O btop.tbz
    tar -xvf btop.tbz
    cd btop && ./install.sh
    cd ..
    rm -r btop
    rm btop.tbz

# if openSUSE
elif cat /etc/os-release | grep -qiE "openSUSE"; then
    sudo zypper install -y python3 python3-pip gedit vim git git-lfs curl wget zsh gcc make perl screen tmux ncdu bat xsel screenfetch neofetch fastfetch p7zip unzip mosh iperf nmap btop

    # install pipx
    python3 -m pip install --user pipx
    python3 -m pipx ensurepath

    # install ctop
    sudo wget https://github.com/bcicen/ctop/releases/download/v0.7.7/ctop-0.7.7-linux-amd64 -O /usr/local/bin/ctop
    sudo chmod +x /usr/local/bin/ctop

    # install vnc
    sudo zypper install -y tigervnc

    # install gitkraken
    wget https://release.gitkraken.com/linux/gitkraken-amd64.rpm
    sudo zypper install --allow-unsigned-rpm -y ./gitkraken-amd64.rpm
    rm gitkraken-amd64.rpm

# if manjaro
elif cat /etc/issue | grep -qiE "Manjaro"; then
    sudo pacman -Sy --noconfirm gedit vim git git-lfs curl wget zsh gcc make perl base-devel binutils screen tmux ncdu bat python-pipx xsel ctop screenfetch neofetch fastfetch p7zip unzip yay mosh iperf3 nmap btop

    # install vncserver
    sudo pacman -Sy --noconfirm tigervnc

    # install gitkraken
    yay -Sy --noconfirm gitkraken

# if arch
elif cat /etc/os-release | grep -qiE "Arch"; then
    sudo pacman -Sy --noconfirm gedit vim git git-lfs curl wget zsh gcc make perl base-devel binutils screen tmux ncdu bat python-pipx xsel ctop screenfetch fastfetch p7zip unzip tigervnc mosh iperf3 nmap btop
    # install yay
    git clone https://aur.archlinux.org/yay.git
    cd yay
    makepkg -si --noconfirm
    cd ..
    rm -rf yay

    # install gitkraken
    yay -Sy --noconfirm gitkraken

# if endeavour os
elif cat /etc/os-release | grep -qiE "EndeavourOS"; then
    sudo pacman -Sy --noconfirm gedit vim git git-lfs curl wget zsh gcc make perl base-devel binutils screen tmux ncdu bat python-pipx xsel ctop screenfetch fastfetch p7zip unzip tigervnc mosh iperf3 nmap btop

    # install gitkraken
    # considering endeavour os cannot use officila docker install script, so we install it here
    yay -Sy --noconfirm gitkraken docker
    sudo systemctl enable docker
    sudo systemctl start docker

else
    echo "Not implemented for the current distro."
    exit
fi

# update pciids
sudo update-pciids

# install fnm for nodejs
curl -o- https://fnm.vercel.app/install | bash

# set tmux color
echo "set -g default-terminal \"screen-256color\"" >> ~/.tmux.conf

# install oh-my-zsh
sh -c "$(curl -fsSL https://raw.githubusercontent.com/ohmyzsh/ohmyzsh/master/tools/install.sh)" "" --unattended

# change default shell
sudo chsh --shell $(which zsh) $(whoami)

# download theme
curl -fsSL https://raw.githubusercontent.com/ControlNet/my-zsh-theme-env/main/files/mzz-ys.zsh-theme > ~/.oh-my-zsh/themes/mzz-ys.zsh-theme

# modify the .zshrc file to change the theme and add plugins
cat ~/.zshrc | sed 's/ZSH_THEME=\"robbyrussell\"/ZSH_THEME=\"mzz-ys\"\nZSH_DISABLE_COMPFIX=\"true\"/' \
    | sed 's/plugins=(git)/plugins=(git zsh-autosuggestions zsh-syntax-highlighting)/' \
    | sed 's/# export PATH=$HOME\/bin:$HOME\/.local\/bin:\/usr\/local\/bin:$PATH/export PATH=$HOME\/.cargo\/bin:$HOME\/.local\/bin:$HOME\/bin:\/usr\/local\/bin:$PATH/' \
    > ~/temp.zshrc

mv ~/temp.zshrc ~/.zshrc

# Disable oh-my-zsh auto update notification
echo "export DISABLE_UPDATE_PROMPT=true" | cat - ~/.zshrc > temp && mv temp ~/.zshrc

# set the TERM=xterm-256color for tmux and Mosh
echo "export TERM=xterm-256color" | cat - ~/.zshrc > temp && mv temp ~/.zshrc

# install other plugins
git clone https://github.com/zsh-users/zsh-autosuggestions ${ZSH_CUSTOM:-~/.oh-my-zsh/custom}/plugins/zsh-autosuggestions
git clone https://github.com/zsh-users/zsh-syntax-highlighting.git ${ZSH_CUSTOM:-~/.oh-my-zsh/custom}/plugins/zsh-syntax-highlighting

# setup git history visualization
git config --global alias.lsd "log --graph --decorate --pretty=oneline --abbrev-commit --all"

# setup git credential
git config --global credential.helper store

# install miniconda
curl -s -L -o miniconda_installer.sh https://repo.anaconda.com/miniconda/Miniconda3-latest-Linux-x86_64.sh
bash miniconda_installer.sh -b
rm miniconda_installer.sh

# initialize conda
~/miniconda3/bin/conda init zsh

# hide conda prefix
echo "changeps1: false" >> ~/.condarc

# setup cargo
curl https://sh.rustup.rs -sSf | sh -s -- -y

# setup go
wget -q -O - https://git.io/vQhTU | bash

# setup lazygit
LAZYGIT_VERSION=$(curl -s "https://api.github.com/repos/jesseduffield/lazygit/releases/latest" | grep -Po '"tag_name": "v\K[^"]*')
curl -Lo lazygit.tar.gz "https://github.com/jesseduffield/lazygit/releases/latest/download/lazygit_${LAZYGIT_VERSION}_Linux_x86_64.tar.gz"
tar xf lazygit.tar.gz lazygit
install lazygit ~/.local/bin/lazygit
rm lazygit.tar.gz lazygit

# setup neovim
curl -LO https://github.com/neovim/neovim/releases/download/v0.9.5/nvim-linux64.tar.gz
tar -xzf nvim-linux64.tar.gz
mv nvim-linux64 ~/.nvim
ln -s $HOME/.nvim/bin/nvim $HOME/.local/bin/nvim
rm nvim-linux64.tar.gz

# setup GoLang environment variables (which is only set for bash in previous)
echo '# GoLang' >> ~/.zshrc
echo 'export GOROOT=$HOME/.go' >> ~/.zshrc
echo 'export PATH=$GOROOT/bin:$PATH' >> ~/.zshrc
echo 'export GOPATH=$HOME/go' >> ~/.zshrc
echo 'export PATH=$GOPATH/bin:$PATH' >> ~/.zshrc

# setup fnm environment variables
echo 'export NFNM_PATH="$HOME/.local/share/fnm"' >> ~/.zshrc
echo 'if [ -d "$FNM_PATH" ]; then' >> ~/.zshrc
echo '  export PATH="$FNM_PATH:$PATH"' >> ~/.zshrc
echo '  eval "`fnm env`"' >> ~/.zshrc
echo 'fi' >> ~/.zshrc
echo 'eval "$(fnm completions --shell zsh)"' >> ~/.zshrc

# install fzf
git clone --depth 1 https://github.com/junegunn/fzf.git ~/.fzf
~/.fzf/install --all

# setup fzf environment variables
echo '[ -f ~/.fzf.zsh ] && source ~/.fzf.zsh' >> ~/.zshrc

# try to apply the changes to the current shell (bash), there will be a problem from omz, but can be ignored
source ~/.zshrc

# install nodejs
fnm install --lts

# setup LunarVim
LV_BRANCH='release-1.4/neovim-0.9' bash <(curl -s https://raw.githubusercontent.com/LunarVim/LunarVim/release-1.4/neovim-0.9/utils/installer/install.sh) -y
~/miniconda3/bin/python -m pip install neovim

# use faster libmamba solver for conda
~/miniconda3/bin/conda install -n base -y conda-libmamba-solver
~/miniconda3/bin/conda config --set solver libmamba

# install jupyter
~/miniconda3/bin/conda install -y ipywidgets ipykernel jupyterlab jupyter

# install Meslo font
wget https://github.com/ryanoasis/nerd-fonts/releases/download/v2.1.0/Meslo.zip
mkdir -p .local/share/fonts
unzip Meslo.zip -d .local/share/fonts
cd .local/share/fonts
rm *Windows*
cd ~
rm Meslo.zip
fc-cache -fv

# install docker - only for Ubuntu, CentOS, and Fedora
if [[ -f /etc/redhat-release ]]; then
    # if fedora or centos
    sh -c "$(curl -fsSL https://get.docker.com)"
elif cat /etc/issue | grep -qiE "Mint|Ubuntu|Pop\!_OS"; then
    # if ubuntu
    sh -c "$(curl -fsSL https://get.docker.com)"
fi
sudo groupadd docker
sudo usermod -aG docker $USER

# setup lazydocker
curl https://raw.githubusercontent.com/jesseduffield/lazydocker/master/scripts/install_update_linux.sh | bash

# install lemonade for neovim/lunarvim clipboard for SSH
go install github.com/lemonade-command/lemonade@latest

# install zerotier
# curl -s https://install.zerotier.com | sudo bash

# install tailscale
# curl -fsSL https://tailscale.com/install.sh | sh

# install lsd with alias to ls
cargo install lsd
echo "alias ls='lsd'" >> ~/.zshrc

# install cargo cache for cleaning cache of cargo
cargo install cargo-cache

# install git-delta
cargo install git-delta

# add duf as the alias to df
go install github.com/muesli/duf@latest
echo "alias df='duf'" >> ~/.zshrc

# add dust as the alias to du
cargo install du-dust
echo "alias du='dust'" >> ~/.zshrc

# add fd as the alias to find
cargo install fd-find
# echo "alias find='fd'" >> ~/.zshrc

# add riggrep as the alias to grep
cargo install --features "pcre2" ripgrep
# echo "alias grep='rg'" >> ~/.zshrc

# install gping as the alias to ping
cargo install gping
echo "alias ping='gping'" >> ~/.zshrc

# install procs as the alias to ps
cargo install procs
echo "alias ps='procs'" >> ~/.zshrc

# install xh (http client)
cargo install xh

# install uv (faster pip)
pipx install uv

# install pixi (conda + poetry)
curl -fsSL https://pixi.sh/install.sh | bash
echo 'eval "$(pixi completion --shell zsh)"' >> ~/.zshrc
mkdir -p ~/.config/pixi
echo "shell.change-ps1 = false" > ~/.config/pixi/config.toml
ln -s ~/.pixi/bin/pixi ~/.local/bin/pixi

# install speedtest-cli (internet speed test)
pipx install speedtest-cli

# install gdown (google drive downloader)
pipx install gdown

# install archey4
pipx install archey4

# install genact
cargo install genact

# install rust-motd, work later
# cargo install --git https://github.com/rust-motd/rust-motd

# install zoxide (better cd)
curl -sS https://raw.githubusercontent.com/ajeetdsouza/zoxide/main/install.sh | bash
echo 'eval "$(zoxide init bash)"' >> ~/.bashrc
echo 'eval "$(zoxide init zsh)"' >> ~/.zshrc

# install micro (better nano)
curl https://getmic.ro | bash
mv micro ~/.local/bin
echo "alias nano='micro'" >> ~/.zshrc

# install scc (code counter)
go install github.com/boyter/scc/v3@latest

# install viu (image viewer)
cargo install viu

# install dive (docker image explorer)
go install github.com/wagoodman/dive@latest

# install tldr (CLI command help)
pipx install tldr

# install huggingface CLI
pipx install "huggingface-hub[cli,hf_xet]"

# install superfile (CLI file manager)
bash -c "$(curl -sLo- https://superfile.netlify.app/install.sh)"
sed -i -E 's/^\s*auto_check_update\s*=.*/auto_check_update = false/' ~/.config/superfile/config.toml

# install yazi (CLI file manager)
cargo install yazi-fm yazi-cli
mkdir -p ~/.config/yazi
ya pack -a BennyOe/onedark
echo '[flavor]\nuse = "onedark"' > ~/.config/yazi/theme.toml

# install pm2
npm config set prefix '~/.local/'
npm install -g pm2

# install CLI agents
npm install -g @openai/codex@latest
npm install -g @google/gemini-cli
curl https://cursor.com/install -fsS | bash
npm install -g @anthropic-ai/claude-code

# install cargo-update
cargo install cargo-update

# install rustscan
cargo install rustscan

# install gotify
go install github.com/gotify/cli@latest
mv ~/go/bin/cli ~/go/bin/gotify

# Monitoring tools

# install bottom (system monitoring)
cargo install bottom

# install nvitop (nvidia gpu monitoring)
pipx install nvitop

# install nviwatch (nvidia gpu monitoring)
cargo install nviwatch

# install bpytop (better htop)
# pipx install bpytop
# echo "alias top='bpytop'" >> ~/.zshrc
# now we use btop instead
echo "alias top='btop'" >> ~/.zshrc

# install bandwhich (bandwidth monitoring)
cargo install bandwhich
sudo install $HOME/.cargo/bin/bandwhich /usr/local/bin

# install rich cli
pipx install rich-cli

# install github CLI
curl -sS https://webi.sh/gh | sh

# install syncthing (file sync)
curl -sS https://webinstall.dev/syncthing | bash
mkdir -p ~/.config/systemd/user
wget https://raw.githubusercontent.com/ControlNet/my-zsh-theme-env/main/files/syncthing.service -O ~/.config/systemd/user/syncthing.service
systemctl --user enable syncthing.service
systemctl --user start syncthing.service

# setup the jupyter systemctl
wget https://raw.githubusercontent.com/ControlNet/my-zsh-theme-env/main/files/jupyter.service -O ~/.config/systemd/user/jupyter.service
systemctl --user enable jupyter.service
systemctl --user start jupyter.service

# run portainer agent
# disable as it requires docker which might not feasible in some cases
sudo docker run -d -p 9001:9001 --name portainer_agent --restart=always -v /var/run/docker.sock:/var/run/docker.sock -v /var/lib/docker/volumes:/var/lib/docker/volumes portainer/agent:latest

cargo cache -a

# change to zsh and apply theme
zsh
