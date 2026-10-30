# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

# Makes intro.wav, the sound played over the engine's intro (src/intro.rs).
# Its timings match the constants there: change them together.
#
#   python3 make_intro_sound.py intro.wav
#
# Needs numpy. Everything is synthesised here, so the sound carries the same
# licence as the rest of the engine.
import numpy as np, wave, sys
SR=44100
FIRST=0.3; LS=0.42; LEVELS=5; NAME_AT=1.7; NAME_FADE=0.6; HOLD=1.0; CLEAR=0.55
sharp_at=FIRST+LEVELS*LS; clear_at=max(sharp_at,NAME_AT+NAME_FADE)+HOLD; total=clear_at+CLEAR
L=total+0.5
n=int(L*SR); t=np.arange(n)/SR
rng=np.random.default_rng(7)
out=np.zeros(n)

def env(a,d,start,dur=None):
    e=np.zeros(n); i0=int(start*SR)
    k=np.arange(n-i0)/SR
    v=np.minimum(k/a,1.0)*np.exp(-np.maximum(k-a,0)/d)
    e[i0:]=v
    return e
def lowpass(x,cut):  # one-pole, cut can be array
    y=np.zeros_like(x); a=np.exp(-2*np.pi*np.broadcast_to(cut,x.shape)/SR); s=0.0
    for i in range(len(x)):
        s=(1-a[i])*x[i]+a[i]*s; y[i]=s
    return y
def crush(x,bits=4,hold=8):
    x=np.repeat(x[::hold],hold)[:len(x)]
    q=2**bits; return np.round(x*q)/q

# 1. Drone rising into the landing.
f=np.interp(t,[0,sharp_at],[52,58])
ph=np.cumsum(f)/SR
saw=lambda p: 2*(p%1)-1
drone=0.5*saw(ph)+0.5*saw(ph*1.007)+0.3*np.sin(2*np.pi*ph*0.5)
cut=np.interp(t,[0,sharp_at,sharp_at+0.3,L],[180,2200,300,200])
drone=lowpass(drone,cut)
amp=np.interp(t,[0,0.25,sharp_at-0.05,sharp_at+0.35,L],[0,0.35,0.6,0.0,0.0])
out+=drone*amp

# 2. A crushed noise bed while it is blocks.
bed=crush(rng.uniform(-1,1,n),bits=3,hold=12)
bamp=np.interp(t,[0,0.15,sharp_at-0.1,sharp_at+0.05,L],[0,0.05,0.09,0.0,0.0])
out+=lowpass(bed,3500)*bamp

# 3. A stuttering burst on each step, each a little higher.
steps=[FIRST+k*LS for k in range(LEVELS)]
for k,s0 in enumerate(steps):
    base=180*2**(k/3)
    reps=3+k%2
    for r in range(reps):
        st=s0+r*0.032; i0=int(st*SR); m=int(0.022*SR)
        if i0+m>n: continue
        tt=np.arange(m)/SR
        fr=base*(1+0.5*rng.random())
        sq=np.sign(np.sin(2*np.pi*fr*tt))
        nz=crush(rng.uniform(-1,1,m),bits=2,hold=4)
        g=(0.55*sq+0.45*nz)*np.exp(-tt/0.012)*(0.5*(0.75**r))
        out[i0:i0+m]+=g
    # a click on the step itself
    i0=int(s0*SR); out[i0:i0+60]+=np.linspace(0.6,0,60)*rng.choice([-1,1])

# 4. The landing: a deep thump and a clean chord, the glitch resolved.
i0=int(sharp_at*SR); m=int(1.2*SR); tt=np.arange(m)/SR
fs=45+75*np.exp(-tt/0.06)
thump=np.sin(2*np.pi*np.cumsum(fs)/SR)*np.exp(-tt/0.35)*0.9
out[i0:i0+m]+=thump[:max(0,min(m,n-i0))]
chord=np.zeros(n)
for fr,a in [(329.63,0.22),(415.30,0.16),(493.88,0.16),(659.26,0.12),(987.77,0.05),(1318.5,0.035)]:
    chord+=a*np.sin(2*np.pi*fr*t+rng.random()*6.28)*(1+0.15*np.sin(2*np.pi*5.5*t))
cenv=env(0.012,1.1,sharp_at)
fade_out=np.interp(t,[0,clear_at,clear_at+CLEAR,L],[1,1,0.25,0])
out+=chord*cenv*fade_out

# A little room: a few combs and allpasses.
def comb(x,d,g):
    y=x.copy(); D=int(d*SR)
    for i in range(D,len(x)): y[i]+=g*y[i-D]
    return y
def allpass(x,d,g):
    y=np.zeros_like(x); D=int(d*SR)
    for i in range(len(x)):
        xd=x[i-D] if i>=D else 0; yd=y[i-D] if i>=D else 0
        y[i]=-g*x[i]+xd+g*yd
    return y
wet=sum(comb(out,d,0.72) for d in [0.0297,0.0371,0.0411,0.0437])/4
wet=allpass(allpass(wet,0.005,0.7),0.0017,0.7)
mix=out+0.22*wet
# Fade the very end, then normalise to -1.5 dB.
mix*=np.interp(t,[0,L-0.25,L],[1,1,0])
mix/=np.max(np.abs(mix))/10**(-1.5/20)
pcm=(mix*32767).astype('<i2')
with wave.open(sys.argv[1],'wb') as w:
    w.setnchannels(1); w.setsampwidth(2); w.setframerate(SR); w.writeframes(pcm.tobytes())
print('seconds',L,'samples',n)
