import subprocess, sys, time
import numpy as np
import rig
link = rig.Link(); cam = rig.Camera()
link.light(); time.sleep(0.9)
dark = np.array(cam.measure()[0])
BARS = " .:-=+*#%@"
def sample(secs):
    t0=time.monotonic(); v=[]
    while time.monotonic()-t0<secs:
        f=cam.frames(1,0)[0]; v.append(f[cam.roi].mean(axis=0)-dark)
    return np.array(v)
def show(tag, v):
    mx=max(8, v.max())
    print(f"{tag}: R {v[:,0].min():.0f}..{v[:,0].max():.0f} G {v[:,1].min():.0f}..{v[:,1].max():.0f} B {v[:,2].min():.0f}..{v[:,2].max():.0f}")
    st=max(1,len(v)//100)
    for c,n in enumerate("RGB"):
        print("   ",n,"|"+"".join(BARS[max(0,min(9,int(x/mx*9.99)))] for x in v[::st,c])+"|")
for kw in (dict(effect=0x51, speed=0x10), dict(effect=0x51, speed=0x10, r=255), dict(effect=0x51, speed=0x05), dict(effect=0x50, r=255)):
    link.light(**kw)
    show(f"{kw} silent 2s", sample(2))
    p=subprocess.Popen(["pw-play","--volume","0.5","beat.wav"])
    time.sleep(0.6)
    show(f"{kw} with 2 Hz beat 4s", sample(4))
    p.terminate(); p.wait()
    time.sleep(0.8)
    print("   state", link.state(), "status", link.query(0x00,"0000008000000080")[16:].hex(" "))
link.light(w=255); time.sleep(0.4); print("restored", link.state())
cam.close(); link.close("music mode")
