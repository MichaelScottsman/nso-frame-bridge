"""Draws the launcher icon: a GameCube-style controller (indigo body, big green
A, red B, kidney X/Y, yellow C-stick), 256x256 RGBA, supersampled."""
import sys
from PIL import Image, ImageDraw

S = 4  # supersampling
W = 256 * S
img = Image.new("RGBA", (W, W), (0, 0, 0, 0))
d = ImageDraw.Draw(img)


def s(*v):
    return [x * S for x in v]


BODY = (104, 96, 196, 255)
BODY_DARK = (74, 66, 160, 255)
GREY = (205, 205, 215, 255)
GREY_DARK = (150, 150, 165, 255)

# Rounded-square backdrop so it reads well in the launcher.
d.rounded_rectangle(s(8, 8, 248, 248), radius=56 * S, fill=(28, 26, 48, 255))

# Grips
d.ellipse(s(30, 120, 100, 222), fill=BODY_DARK)
d.ellipse(s(156, 120, 226, 222), fill=BODY_DARK)
# Body
d.rounded_rectangle(s(26, 70, 230, 176), radius=50 * S, fill=BODY)
d.ellipse(s(30, 118, 96, 212), fill=BODY)
d.ellipse(s(160, 118, 226, 212), fill=BODY)

# Main stick (left)
d.ellipse(s(40, 84, 96, 140), fill=BODY_DARK)
d.ellipse(s(48, 92, 88, 132), fill=GREY_DARK)
d.ellipse(s(54, 98, 82, 126), fill=GREY)

# D-pad (lower left)
d.ellipse(s(72, 138, 112, 178), fill=BODY_DARK)
d.rectangle(s(87, 144, 97, 172), fill=GREY_DARK)
d.rectangle(s(78, 153, 106, 163), fill=GREY_DARK)

# C-stick (lower right)
d.ellipse(s(144, 138, 184, 178), fill=BODY_DARK)
d.ellipse(s(152, 146, 176, 170), fill=(245, 204, 40, 255))

# Start
d.ellipse(s(121, 104, 135, 118), fill=GREY)

# Face buttons: big green A, red B, grey X / Y
d.ellipse(s(170, 92, 206, 128), fill=(40, 190, 120, 255))
d.ellipse(s(150, 120, 168, 138), fill=(222, 60, 60, 255))
# X / Y: kidney-shaped arcs hugging A (centre 188,110) on the right and top.
d.arc(s(162, 84, 214, 136), start=-70, end=30, fill=GREY, width=11 * S)    # X
d.arc(s(162, 84, 214, 136), start=195, end=275, fill=GREY, width=11 * S)   # Y

out = sys.argv[1] if len(sys.argv) > 1 else "icon.png"
img.resize((256, 256), Image.LANCZOS).save(out)
print("wrote", out)
