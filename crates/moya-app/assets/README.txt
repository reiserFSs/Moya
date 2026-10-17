Moya logo assets
================
Source: source.png, 1254x1254 RGB, supplied Luminous Eclipse Halo Icon.
All generated PNGs are RGBA with fully transparent exterior corners and antialiased edges.
No project files were changed by asset generation.

App icon masking
----------------
The tile was detected from source pixels whose maximum RGB channel exceeds 12/255,
not from a hard-coded crop. Median central row/column edges measured the bounds
x=97..1155, y=92..1118. A rounded-rectangle corner fit against the detected row
edges measured radius 243 source pixels (combined median edge residual 2.82 px).
A signed-distance rounded-rectangle mask supplies an approximately one-source-pixel
feather. Original RGB and all ring/glow pixels inside the tile remain intact.
The slight original rectangular aspect ratio is preserved. Lanczos resizing centers
the tile at approximately 824x799 pixels on a transparent 1024x1024 macOS grid.

Small logos
-----------
The toolbar and UXP icons use the ring alone, not the dark tile: the glowing ring
reads more clearly on a dark toolbar at small sizes. A screen-style extraction
subtracts the dark 24/255 background floor and transfers brightness to alpha;
straight RGB is recovered so the glow composites correctly over dark UI.
The visible glow crop was detected at >48/255, padded 12%, with a gentle perimeter
fade. A radial fade derived from the bright ring's measured center/radius removes
faint remnants of the tile edge. This does not clip the bright ring. Lanczos
resampling retains the warm glow; no drawn replacement or generic icon is used.

Files
-----
source.png: supplied original (1254x1254 RGB).
icon-1024.png: macOS-grid app artwork, 1024x1024 RGBA; 1,109,449 bytes.
window-icon-256.png: matching viewport icon, 256x256 RGBA; 59,494 bytes.
header-logo-64.png: transparent ring-only toolbar logo, 64x64 RGBA; 6,180 bytes.
header-logo-128.png: transparent ring-only toolbar logo, 128x128 RGBA; 22,241 bytes.
ps-plugin/icons/panel-23.png: panel icon @1x, 23x23 RGBA; 1,153 bytes.
ps-plugin/icons/panel-46.png: panel icon @2x, 46x46 RGBA; 3,503 bytes.
ps-plugin/icons/plugin-48.png: plugin-list icon @1x, 48x48 RGBA; 3,745 bytes.
ps-plugin/icons/plugin-96.png: plugin-list icon @2x, 96x96 RGBA; 12,903 bytes.
Moya.iconset/icon_16x16.png: 16x16; 688 bytes.
Moya.iconset/icon_16x16@2x.png: 32x32; 1,801 bytes.
Moya.iconset/icon_32x32.png: 32x32; 1,801 bytes.
Moya.iconset/icon_32x32@2x.png: 64x64; 5,216 bytes.
Moya.iconset/icon_128x128.png: 128x128; 16,410 bytes.
Moya.iconset/icon_128x128@2x.png: 256x256; 59,494 bytes.
Moya.iconset/icon_256x256.png: 256x256; 59,494 bytes.
Moya.iconset/icon_256x256@2x.png: 512x512; 258,258 bytes.
Moya.iconset/icon_512x512.png: 512x512; 258,258 bytes.
Moya.iconset/icon_512x512@2x.png: 1024x1024; 1,109,449 bytes.
Moya.icns: native icon container, 1,894,615 bytes, produced successfully by:
  iconutil -c icns /tmp/moya-assets/Moya.iconset -o /tmp/moya-assets/Moya.icns

Verification
------------
Every generated PNG was opened individually with the image read tool. Images below
200px are automatically enlarged by that viewer, which also reports original sizes.
Numeric checks verified all 18 generated PNGs are RGBA, have alpha=0 at all four
corners, and contain fractional alpha for smooth edges. The ring-only logo was also
composited over a dark background for inspection. Transparent-image previews may
show saturated straight-RGB fringes where alpha is almost zero; correct alpha
compositing gives the original warm glow, not those preview fringes.
