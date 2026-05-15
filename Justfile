run $GST_DEBUG="GST_CAPS:5,pipewiresrc:5":
    cargo run

[arg('board', pattern='radxa-dragon-q6a')]
[working-directory('device-tree')]
build-dtb board:
    if [ ! -d "./build" ]; then echo "Cloning build"; git clone https://github.com/armbian/build; fi
    # podman run --rm --userns=keep-id -it --entrypoint /bin/bash -v .:/root/armbian:z docker.io/armbian/build:latest ./build.sh {{ board }}
    ./build.sh {{ board }}
