run $GST_DEBUG="GST_CAPS:5,pipewiresrc:5":
    cargo run

[arg('board', pattern='radxa-dragon-q6a')]
[arg('command', pattern='kernel|kernel-config|kernel-patch|dts-check|kernel-dtb')]
[working-directory('armbian')]
armbian-build board command: chown-armbian-build-volumes _mount_armbian_tmpfs _build-armbian-container
    podman run --rm -it -v ./output:/armbian/output:z -v ./cache:/armbian/cache:z -v ./tmp/:/armbian/.tmp:z armbian-build-carjack ./compile.sh BOARD={{ board }} BRANCH=edge KERNEL_GIT=full {{ command }}

[working-directory('armbian')]
armbian-shell: chown-armbian-build-volumes _mount_armbian_tmpfs _build-armbian-container
    podman run --rm -it -v ./output:/armbian/output:z -v ./cache:/armbian/cache:z -v ./tmp/:/armbian/.tmp:z armbian-build-carjack /bin/bash

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
clean-armbian-build:
    sudo rm -rf output/*
    sudo rm -rf cache/*
    sudo rm -rf tmp/*

[working-directory('armbian')]
_setup-armbian-build-dirs:
    mkdir -p output
    mkdir -p cache
    mkdir -p tmp

_build-armbian-container:
    podman build -t armbian-build-carjack .

[working-directory('armbian')]
fetch-armbian-build:
    echo "Cloning armbian build system"
    git clone https://github.com/armbian/build

copy-kernel-to-fishball:
    scp armbian/output/debs/* fishball:/home/fishball/
