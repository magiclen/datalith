#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

version=7.1.2-32
checksum=940e349f0ef394e658fd57400b83d1d7a81b954f6e7bdbfbfad31b2718c10add
build_dir=$(mktemp -d)
trap 'rm -rf "$build_dir"' EXIT

sudo apt-get update
# Use the same image delegates as the Docker image; Ubuntu only suggests the HEVC decoder of libheif, so install it explicitly.
sudo apt-get install -y --no-install-recommends build-essential curl pkg-config clang libmagic-dev libjpeg-dev libpng-dev libwebp-dev libtiff-dev zlib1g-dev \
    libheif-dev libheif-plugin-libde265 libjxl-dev libopenjp2-7-dev liblcms2-dev libopenexr-dev libraw-dev libdjvulibre-dev \
    libbz2-dev liblzma-dev libzstd-dev fonts-dejavu-core
curl --fail --location --retry 3 "https://github.com/ImageMagick/ImageMagick/archive/refs/tags/${version}.tar.gz" -o "$build_dir/imagemagick.tar.gz"
printf '%s  %s\n' "$checksum" "$build_dir/imagemagick.tar.gz" | sha256sum --check -
tar xzf "$build_dir/imagemagick.tar.gz" -C "$build_dir"
cd "$build_dir/ImageMagick-$version"
./configure --enable-hdri --with-quantum-depth=16 --disable-static --disable-docs --without-perl --without-x \
    --with-webp --with-heic --with-jxl --with-openjp2 --with-lcms --with-openexr --with-raw --with-djvu \
    --without-rsvg --without-gvc
make -j"$(nproc)"
sudo make install
# Run the tests with the same security policy as the Docker image.
sudo install -m 644 "$script_dir/../../datalith-core/src/service/image_policy.xml" /usr/local/etc/ImageMagick-7/policy.xml
sudo ldconfig
sh "$script_dir/check-imagemagick.sh"
ffmpeg -version
