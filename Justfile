run $GST_DEBUG="GST_CAPS:5,pipewiresrc:5":
    cargo run

podman_run_armbian := "podman run --rm -it -v ~/.gitconfig:/armbian/.gitconfig:z -v ./output:/armbian/output:z -v ./cache:/armbian/cache:z -v ./tmp/:/armbian/.tmp:z armbian-build-carjack"

[arg('board', pattern='radxa-dragon-q6a')]
[arg('command', pattern='kernel|kernel-config|kernel-patch|dts-check|kernel-dtb')]
[working-directory('armbian')]
armbian-build board command: chown-armbian-build-volumes _mount_armbian_tmpfs _build-armbian-container
    rm -rf output/*
    {{ podman_run_armbian }} ./compile.sh BOARD={{ board }} BRANCH=edge KERNEL_GIT=full {{ command }}

[working-directory('armbian')]
armbian-shell: chown-armbian-build-volumes _mount_armbian_tmpfs _build-armbian-container
    {{ podman_run_armbian }} /bin/bash

[script]
[working-directory('armbian')]
_mount_armbian_tmpfs:
    uid=`id -u`
    gid=`id -g`
    tmpdir="$PWD"/tmp
    sudo mount -t tmpfs -o size=12G,mode=0777,uid="$uid",gid="$gid" tmpfs "$tmpdir"

[working-directory('armbian')]
chown-armbian-build-volumes:
    sudo --preserve-env chown -R $USER:$USER output
    sudo --preserve-env chown -R $USER:$USER cache
    sudo --preserve-env chown -R $USER:$USER tmp

[working-directory('armbian')]
clean-armbian-build: chown-armbian-build-volumes
    rm -rf output/*
    rm -rf cache/*
    rm -rf tmp/*

[working-directory('armbian')]
_setup-armbian-build-dirs:
    mkdir -p output
    mkdir -p cache
    mkdir -p tmp

[working-directory('armbian')]
_build-armbian-container: _fetch-armbian-build
    podman build -t armbian-build-carjack .

[script]
[working-directory('armbian')]
_fetch-armbian-build:
    if [ -d "./build" ]; then
        echo "Updating armbian build system"
        cd build && git pull
    else
        echo "Cloning armbian build system"
        git clone https://github.com/cfraz9/build
    fi

[working-directory('armbian')]
install-kernel-on-fishball:
    rsync -ravzP --delete output/debs fishball:~
    ssh root@fishball "dpkg -i /home/fishball/debs/*.deb"
