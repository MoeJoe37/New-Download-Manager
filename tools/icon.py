# Picture on black -> transparent square PNG (+ .ico). Black is removed only where it touches
# the border (flood fill), so dark shading inside the art stays. usage: icon.py src.png out.png [out.ico]
import sys, numpy as np
from PIL import Image
from scipy import ndimage
a = np.asarray(Image.open(sys.argv[1]).convert('RGB')).astype(float)
v = a.max(axis=2)
bg = ndimage.label(v < 40)[0]
edge = set(np.unique(np.r_[bg[0], bg[-1], bg[:, 0], bg[:, -1]])) - {0}
mask = np.isin(bg, list(edge))
alpha = np.where(mask, 0, 255).astype(float)
# soft edge: fade the 2 px ring next to the background by brightness
ring = ndimage.binary_dilation(mask, iterations=2) & ~mask
alpha[ring] = np.clip(v[ring] * 4, 0, 255)
img = Image.fromarray(np.dstack([a, alpha]).astype(np.uint8), 'RGBA')
img = img.crop(img.getbbox())
s = max(img.size); sq = Image.new('RGBA', (s, s)); sq.paste(img, ((s - img.width) // 2, (s - img.height) // 2))
sq = sq.resize((256, 256), Image.LANCZOS)
sq.save(sys.argv[2])
if len(sys.argv) > 3:
    sq.save(sys.argv[3], sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
