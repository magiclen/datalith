#!/usr/bin/env bash
set -euo pipefail

# Build the same media tools for CI and the full Docker image.
ffmpeg_version=9.0.2
ffmpeg_checksum=8c3850283eb25fa026482078a04051e0be17347b09ef81a0849bec15a96e002e
x264_revision=b35605ace3ddf7c1a5d67a2eb553f034aef41d55
x264_checksum=cd71a7515b0e9a012e1ac9b1f8415bebcaf6fc97d4db32286642ac4c0fbe24f9
zimg_version=3.0.6
zimg_checksum=be89390f13a5c9b2388ce0f44a5e89364a20c1c57ce46d382b1fcc3967057577
webp_version=1.6.0
webp_checksum=e4ab7009bf0629fd11982d4c2aa83964cf244cffba7347ecd39019a9e38c4564
script_path=$(realpath "${BASH_SOURCE[0]}")
script_dir=$(dirname "$script_path")
installer_license="$script_dir/LICENSE"
if [[ ! -f "$installer_license" ]]; then
    installer_license="$script_dir/../../LICENSE"
fi

prefix=${FFMPEG_PREFIX:-/opt/ffmpeg}
jobs=${FFMPEG_BUILD_JOBS:-2}
if [[ "$prefix" != /* || ! "$jobs" =~ ^[1-9][0-9]*$ ]]; then
    printf '%s\n' 'FFMPEG_PREFIX must be absolute and FFMPEG_BUILD_JOBS must be a positive integer.' >&2
    exit 1
fi

if [[ ${FFMPEG_INSTALL_DEPS:-0} == 1 ]]; then
    # Use this option as root on a Debian or Ubuntu CI runner.
    apt-get update
    apt-get install -y --no-install-recommends build-essential ca-certificates curl pkg-config nasm autoconf automake libtool zlib1g-dev xz-utils bzip2
    rm -rf /var/lib/apt/lists/*
fi

build_dir=$(mktemp -d)
trap 'rm -rf "$build_dir"' EXIT
download_dir=${FFMPEG_DOWNLOAD_DIR:-$build_dir/downloads}
mkdir -p "$download_dir" "$prefix"
download_dir=$(cd "$download_dir" && pwd)

download() {
    local filename=$1 checksum=$2 url=$3
    if [[ ! -f "$download_dir/$filename" ]]; then
        curl --fail --location --retry 3 "$url" -o "$download_dir/$filename"
    fi
    printf '%s  %s\n' "$checksum" "$download_dir/$filename" | sha256sum --check -
}

ffmpeg_archive="ffmpeg-$ffmpeg_version.tar.xz"
x264_archive="x264-$x264_revision.tar.gz"
zimg_archive="zimg-release-$zimg_version.tar.gz"
webp_archive="libwebp-$webp_version.tar.gz"
download "$ffmpeg_archive" "$ffmpeg_checksum" "https://ffmpeg.org/releases/$ffmpeg_archive"
# The fixed mirror archive matches the upstream source tree and avoids GitLab download challenges.
download "$x264_archive" "$x264_checksum" "https://github.com/mirror/x264/archive/$x264_revision.tar.gz"
download "$zimg_archive" "$zimg_checksum" "https://github.com/sekrit-twc/zimg/archive/refs/tags/release-$zimg_version.tar.gz"
download "$webp_archive" "$webp_checksum" "https://storage.googleapis.com/downloads.webmproject.org/releases/webp/$webp_archive"

tar xf "$download_dir/$ffmpeg_archive" -C "$build_dir"
tar xf "$download_dir/$x264_archive" -C "$build_dir"
tar xf "$download_dir/$zimg_archive" -C "$build_dir"
tar xf "$download_dir/$webp_archive" -C "$build_dir"

# Keep the external codec libraries private and link them into the media tools.
codec_prefix="$build_dir/codecs"
cd "$build_dir/x264-$x264_revision"
./configure --prefix="$codec_prefix" --enable-static --enable-pic --disable-cli --disable-opencl
make -j"$jobs"
make install

cd "$build_dir/zimg-release-$zimg_version"
./autogen.sh
./configure --prefix="$codec_prefix" --enable-static --disable-shared --with-pic
make -j"$jobs"
make install

cd "$build_dir/libwebp-$webp_version"
./configure --prefix="$codec_prefix" --enable-static --disable-shared --with-pic \
    --disable-gl --disable-png --disable-jpeg --disable-tiff --disable-gif
make -j"$jobs"
make install

cd "$build_dir/ffmpeg-$ffmpeg_version"
if ! PKG_CONFIG_PATH="$codec_prefix/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}" ./configure \
    --prefix="$prefix" --disable-autodetect --disable-debug --disable-doc --disable-ffplay \
    --disable-shared --enable-static --disable-network --enable-gpl --enable-libx264 --enable-libzimg --enable-libwebp \
    --enable-zlib --pkg-config-flags=--static --extra-libs=-lm; then
    tail -n 100 ffbuild/config.log >&2
    exit 1
fi
make -j"$jobs"
make install-progs

# The runtime ships the exact source archives and the build script with the GPL tools.
share_dir="$prefix/share/datalith-ffmpeg"
source_dir="$build_dir/source"
mkdir -p "$share_dir/licenses" "$source_dir"
cp "$download_dir/$ffmpeg_archive" "$download_dir/$x264_archive" "$download_dir/$zimg_archive" "$download_dir/$webp_archive" "$source_dir/"
cp "$script_path" "$source_dir/install-ffmpeg.sh"
cp "$installer_license" "$source_dir/LICENSE"
cp "$installer_license" "$share_dir/licenses/datalith-installer-LICENSE"
cp LICENSE.md COPYING.* "$share_dir/licenses/"
cp "$build_dir/x264-$x264_revision/COPYING" "$share_dir/licenses/x264-COPYING"
cp "$build_dir/zimg-release-$zimg_version/COPYING" "$share_dir/licenses/zimg-COPYING"
cp "$build_dir/libwebp-$webp_version/COPYING" "$share_dir/licenses/libwebp-COPYING"
cp "$build_dir/libwebp-$webp_version/PATENTS" "$share_dir/licenses/libwebp-PATENTS"
cp "$build_dir/libwebp-$webp_version/AUTHORS" "$share_dir/licenses/libwebp-AUTHORS"
printf '%s  %s\n' "$ffmpeg_checksum" "$ffmpeg_archive" "$x264_checksum" "$x264_archive" "$zimg_checksum" "$zimg_archive" "$webp_checksum" "$webp_archive" > "$source_dir/SHA256SUMS"
{
    printf 'FFmpeg: %s\nx264: %s\nzimg: %s\nlibwebp: %s\n' "$ffmpeg_version" "$x264_revision" "$zimg_version" "$webp_version"
    printf 'Build jobs: %s\n' "$jobs"
    "$prefix/bin/ffmpeg" -buildconf 2>&1
    cc --version
    c++ --version
    if command -v dpkg-query >/dev/null 2>&1; then
        dpkg-query -W
    fi
} > "$share_dir/build-info.txt"
cp "$share_dir/build-info.txt" "$source_dir/"
tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner -cJf "$share_dir/ffmpeg-source.tar.xz" -C "$source_dir" .

"$prefix/bin/ffmpeg" -version
"$prefix/bin/ffprobe" -version
