#!/bin/sh
# Rust-only Docker stages also use this workspace wrapper, without Node.
# The release package overrides select the measured O3/four-unit configuration.
opt_level=
codegen_units=
explicit_llvm_args=no
passthrough=no
codegen_next=no

for argument do
    case "$argument" in
        --print|--print=*|--version|-V|-vV|--help|-h|@*) passthrough=yes ;;
    esac

    if [ "$codegen_next" = yes ]; then
        codegen=$argument
        codegen_next=no
    else
        case "$argument" in
            -C|--codegen) codegen_next=yes; continue ;;
            -C*) codegen=${argument#-C} ;;
            --codegen=*) codegen=${argument#--codegen=} ;;
            *) continue ;;
        esac
    fi

    case "$codegen" in
        opt-level=*|opt_level=*) opt_level=${codegen#*=} ;;
        codegen-units=*|codegen_units=*) codegen_units=${codegen#*=} ;;
        llvm-args=*|llvm_args=*) explicit_llvm_args=yes ;;
    esac
done

# Response files can override the options above; leave their expansion to rustc.
# Explicit LLVM arguments also give callers full control over their configuration.
if [ "$passthrough" = no ] && [ "$explicit_llvm_args" = no ] \
    && [ "$opt_level" = 3 ] && [ "$codegen_units" = 4 ]; then
    exec "$@" -C 'llvm-args=--inline-threshold=75 --inlinehint-threshold=75 --locally-hot-callsite-threshold=75'
fi

exec "$@"
