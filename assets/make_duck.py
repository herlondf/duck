"""Gera o icone do Duck (PNG + ICO) e a grade de pixels da apresentacao a
partir das formas do duck-animal-svgrepo-com.svg (viewBox 512).

    python assets/make_duck.py
"""
from PIL import Image, ImageDraw
import os

S = 4  # supersampling
W = 512 * S
here = os.path.dirname(os.path.abspath(__file__))

YELLOW, YELLOW2 = (0xFF, 0xC8, 0x43), (0xF3, 0xB7, 0x3F)
BLUE, BLUE2 = (0x6C, 0xCF, 0xE2), (0x58, 0xA4, 0xB3)
EYE = (0x4A, 0x50, 0x52)


def k(*v):
    return [x * S for x in v]


def draw():
    img = Image.new('RGBA', (W, W), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)
    # bico: retangulo com canto inferior direito bem redondo
    d.rounded_rectangle(k(379.4, 144.8, 512, 229.0), radius=16 * S, fill=YELLOW)
    d.rectangle(k(440, 190, 512, 229), fill=(0, 0, 0, 0))
    d.pieslice(k(376, 93, 512, 229), 0, 90, fill=YELLOW)
    d.rectangle(k(379.4, 160, 444, 229), fill=YELLOW)
    # cabeca: topo em semicirculo
    d.ellipse(k(247.7, 57.5, 412.1, 222), fill=BLUE)
    d.rounded_rectangle(k(247.7, 139.7, 412.1, 264.7), radius=16 * S, fill=BLUE)
    # corpo: meio disco + faixa de cima
    d.pieslice(k(0, 42.3, 412.1, 454.5), 0, 180, fill=BLUE)
    d.rounded_rectangle(k(0, 232, 412.1, 260), radius=16 * S, fill=BLUE)
    # sombra da metade direita do corpo
    d.pieslice(k(0, 42.3, 412.1, 454.5), 0, 90, fill=BLUE2)
    d.rectangle(k(206, 232, 412.1, 249), fill=BLUE2)
    # asa (meia elipse) e sua sombra
    d.pieslice(k(91, 213.3, 321, 386.6), 0, 180, fill=YELLOW)
    d.pieslice(k(91, 213.3, 321, 386.6), 0, 90, fill=YELLOW2)
    # olho
    d.ellipse(k(323.5, 113.2, 366.8, 156.4), fill=EYE)
    return img


def main():
    img = draw()
    big = img.resize((256, 256), Image.LANCZOS)
    big.save(os.path.join(here, 'duck.png'))
    img.resize((256, 256), Image.LANCZOS).save(os.path.join(here, 'duck.ico'),
        sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])
    # Grade de pixels (arte "ASCII" da apresentacao): uma letra por cor.
    n = 26
    small = img.resize((n, n), Image.BOX)
    pal = {'y': YELLOW, 'Y': YELLOW2, 'b': BLUE, 'B': BLUE2, 'e': EYE}
    rows = []
    for yy in range(n):
        row = ''
        for xx in range(n):
            r, g, b, a = small.getpixel((xx, yy))
            if a < 110:
                row += '.'
                continue
            row += min(pal, key=lambda c: sum((p - q) ** 2 for p, q in zip(pal[c], (r, g, b))))
        rows.append(row)
    while rows and set(rows[0]) == {'.'}: rows.pop(0)
    while rows and set(rows[-1]) == {'.'}: rows.pop()
    open(os.path.join(here, 'duck.txt'), 'w').write('\n'.join(rows) + '\n')
    print('\n'.join(rows))


if __name__ == '__main__':
    main()
