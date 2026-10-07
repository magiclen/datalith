#!/usr/bin/env sh
set -eu

# `--with-X` skips a missing library without failing, so check the delegates of the built ImageMagick.
delegates=" $(magick -list configure | awk '$1 == "DELEGATES" { $1 = ""; print }') "
for delegate in djvu heic jp2 jpeg jxl lcms openexr png raw tiff webp; do
    case $delegates in
        *" $delegate "*) ;;
        *)
            echo "ImageMagick is missing the $delegate delegate." >&2
            exit 1
            ;;
    esac
done
# Datalith renders SVG with resvg, and these delegates can read other files or are unmaintained.
for delegate in gvc rsvg wmf; do
    case $delegates in
        *" $delegate "*)
            echo "ImageMagick must not use the $delegate delegate." >&2
            exit 1
            ;;
    esac
done

# Every library of ImageMagick must be installed.
if ldd "$(command -v magick)" | grep 'not found'; then
    exit 1
fi
magick -version
