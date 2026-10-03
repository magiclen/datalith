#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

version=7.1.2-32
checksum=940e349f0ef394e658fd57400b83d1d7a81b954f6e7bdbfbfad31b2718c10add
build_dir=$(mktemp -d)
trap 'rm -rf "$build_dir"' EXIT

sudo apt-get update
sudo apt-get install -y --no-install-recommends build-essential curl pkg-config clang libmagic-dev libjpeg-dev libpng-dev libwebp-dev librsvg2-dev libtiff-dev zlib1g-dev ffmpeg
curl --fail --location --retry 3 "https://github.com/ImageMagick/ImageMagick/archive/refs/tags/${version}.tar.gz" -o "$build_dir/imagemagick.tar.gz"
printf '%s  %s\n' "$checksum" "$build_dir/imagemagick.tar.gz" | sha256sum --check -
tar xzf "$build_dir/imagemagick.tar.gz" -C "$build_dir"
cd "$build_dir/ImageMagick-$version"
./configure --enable-hdri --with-quantum-depth=16 --with-webp --with-rsvg --disable-static --disable-docs --without-perl --without-x
make -j"$(nproc)"
sudo make install
# Run the tests with the same security policy as the Docker image.
sudo install -m 644 "$script_dir/../../datalith-core/src/service/image_policy.xml" /usr/local/etc/ImageMagick-7/policy.xml
sudo ldconfig
magick -version
ffmpeg -version
