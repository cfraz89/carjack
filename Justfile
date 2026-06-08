set dotenv-load
mod armbian

run $GST_DEBUG="GST_CAPS:5,pipewiresrc:5":
    cargo run

[script]
build:
    if [ -z "$BUILD_RUNNER" ]; then
        cargo build;
    else
        "$BUILD_RUNNER" run cargo build
    fi