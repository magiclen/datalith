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
# The delegate list does not show webpmux, but without it the WebP coder cannot read or write more than one frame.
if ! magick -list format | awk '($1 == "WEBP" || $1 == "WEBP*") && $2 == "rw+" { found = 1 } END { exit !found }'; then
    echo "ImageMagick is missing the webpmux library for animated WebP." >&2
    exit 1
fi
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
