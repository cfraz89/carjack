set dotenv-load
mod armbian

run $GST_DEBUG="GST_CAPS:5,pipewiresrc:5": build
    cargo run

[script]
build-catplay: 
    cd catplay/core/catplay_hap
    if [ -z "$BUILD_RUNNER" ]; then
        cargo build;
    else
        "$BUILD_RUNNER" run cargo build
    fi

[script]
build: build-catplay
    if [ -z "$BUILD_RUNNER" ]; then
        cargo build;
    else
        "$BUILD_RUNNER" run cargo build
    fi