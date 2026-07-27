#!/usr/bin/env sh
set -eu

binary=${1:?usage: package-unix.sh <binary> <target-name> [output-directory]}
target_name=${2:?usage: package-unix.sh <binary> <target-name> [output-directory]}
output_directory=${3:-dist}
package_directory=$(mktemp -d)

cleanup() {
  rm -rf "$package_directory"
}
trap cleanup EXIT HUP INT TERM

mkdir -p "$output_directory"
cp "$binary" "$package_directory/structurely"
cp README.md LICENSE "$package_directory/"
tar -czf "$output_directory/structurely-$target_name.tar.gz" -C "$package_directory" .
