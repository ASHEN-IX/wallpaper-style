#!/usr/bin/env python3
"""Follow the active KDE Plasma wallpaper and derive a dark-theme accent."""
import argparse
import colorsys
import configparser
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import sys
import time
from urllib.parse import unquote, urlparse

SCRIPT = '''print(JSON.stringify(desktops().map(function(d){d.currentConfigGroup=["Wallpaper",d.wallpaperPlugin,"General"];return {screen:d.screen,plugin:d.wallpaperPlugin,image:d.readConfig("Image","")};})))'''

def wallpaper():
    result = subprocess.run(['qdbus6','org.kde.plasmashell','/PlasmaShell','org.kde.PlasmaShell.evaluateScript',SCRIPT],capture_output=True,text=True,check=True,timeout=10)
    entries = json.loads(result.stdout.strip())
    for item in sorted(entries, key=lambda x: x.get('screen', 999)):
        image = item.get('image', '')
        uri = urlparse(image)
        if uri.scheme == 'file' and uri.netloc in ('', 'localhost'):
            path = Path(unquote(uri.path))
            if path.is_file(): return path
    raise RuntimeError('No local image wallpaper found; choose an image wallpaper in Plasma.')

def choose_accent(pixels):
    # Prefer colors that are both present and reasonably saturated, ignoring
    # black/white regions that would make highlights hard to distinguish.
    buckets = {}
    for i in range(0, len(pixels)-2, 3):
        r,g,b = (v / 255 for v in pixels[i:i+3])
        h,s,v = colorsys.rgb_to_hsv(r,g,b)
        if s < .18 or v < .12: continue
        bucket = int(h * 24) % 24
        weight = s * (.4 + .6*v)
        n, total = buckets.get(bucket, (0, 0.0))
        buckets[bucket] = n+1, total+weight
    if not buckets: return (61, 220, 151)
    winner = max(buckets, key=lambda k: buckets[k][1])
    rgb = colorsys.hls_to_rgb((winner + .5)/24, .62, .65)
    return tuple(round(v*255) for v in rgb)

def extract(path):
    cmd = ['magick', str(path)+'[0]', '-auto-orient', '-resize', '48x48!', '-alpha', 'off', '-depth', '8', 'rgb:-']
    result = subprocess.run(cmd, capture_output=True, check=True, timeout=30)
    if len(result.stdout) != 48*48*3: raise RuntimeError('Image conversion returned unexpected data')
    return choose_accent(result.stdout)

def settings():
    path = Path(os.environ.get('XDG_CONFIG_HOME', str(Path.home()/'.config')))/'wallpaper-style/settings.json'
    if not path.exists(): return {"opacity": {"default": 0.80, "browser": 0.82, "launcher": 0.85}, "kvantum_apps": ["dolphin", "kate", "kwrite", "ark"], "kvantum": True}
    with path.open() as f: return json.load(f)

def rgb_hex(rgb): return '#%02x%02x%02x' % rgb

def make_kvantum(rgb, opts):
    """Derive per-user Kvantum themes from the user's active theme, if installed."""
    if not opts.get('kvantum', True): return False
    root=Path(os.environ.get('XDG_CONFIG_HOME',str(Path.home()/'.config')))/'Kvantum'
    configfile=root/'kvantum.kvconfig'
    if not configfile.exists():
        print('Kvantum not configured; Plasma colors and transparency still work.',file=sys.stderr,flush=True); return False
    cfg=configparser.ConfigParser(interpolation=None,strict=False);cfg.optionxform=str;cfg.read(configfile)
    if not cfg.has_section('General'): return False
    state=Path(os.environ.get('XDG_STATE_HOME',str(Path.home()/'.local/state')))/'wallpaper-style';state.mkdir(parents=True,exist_ok=True)
    basefile=state/'base-theme.json'
    if basefile.exists(): base=json.loads(basefile.read_text())['theme']
    else:
        base=cfg['General'].get('theme','')
        basefile.write_text(json.dumps({'theme':base})+'\n')
    candidates=[root/base,Path('/usr/share/Kvantum')/base]
    src=next((p for p in candidates if (p/(base+'.kvconfig')).is_file() and (p/(base+'.svg')).is_file()),None)
    if src is None:
        print(f'Kvantum base theme {base!r} not found; skipping theme generation.',file=sys.stderr,flush=True);return False
    old_config=(src/(base+'.kvconfig')).read_text()
    old_svg=(src/(base+'.svg')).read_text()
    matched=re.search(r'^highlight\.color=(#[0-9a-fA-F]{6})',old_config,re.M)
    old_accent=matched.group(1) if matched else '#3ddc97'
    accent=rgb_hex(rgb)
    h,_,sat=colorsys.rgb_to_hls(*(v/255 for v in rgb))
    bg=rgb_hex(tuple(round(x*255) for x in colorsys.hls_to_rgb(h,.075,min(sat,.20))))
    panel=rgb_hex(tuple(round(x*255) for x in colorsys.hls_to_rgb(h,.13,min(sat,.18))))
    base_name='WallpaperStyle'+accent[1:].upper()
    for name,transparent in [(base_name,False),(base_name+'Glass',True)]:
        dest=root/name;dest.mkdir(parents=True,exist_ok=True)
        config=old_config
        svg=old_svg
        for before,after in [(old_accent,accent),('#3ddc97',accent),('#111418',bg),('#1b2127',panel),('#181825',bg),('#1e1e2e',panel)]:
            config=re.sub(re.escape(before),after,config,flags=re.I)
            svg=re.sub(re.escape(before),after,svg,flags=re.I)
        if transparent:
            for key,value in [('translucent_windows','true'),('blurring','true'),('reduce_window_opacity','20'),('reduce_menu_opacity','20')]:
                if re.search(r'^'+key+r'=.*$',config,re.M): config=re.sub(r'^'+key+r'=.*$',key+'='+value,config,flags=re.M)
                else: config=config.replace('[%General]','[%General]\n'+key+'='+value,1)
        (dest/(name+'.svg')).write_text(svg)
        (dest/(name+'.kvconfig')).write_text(config)
    cfg['General']['theme']=base_name
    if not cfg.has_section('Applications'): cfg.add_section('Applications')
    apps=opts.get('kvantum_apps',[])
    # Remove only the chosen applications from previous per-app mappings.
    for name,value in list(cfg['Applications'].items()):
        remaining=[v.strip() for v in value.split(',') if v.strip() and v.strip() not in apps]
        if remaining: cfg['Applications'][name]=', '.join(remaining)
        else: cfg.remove_option('Applications',name)
    if apps: cfg['Applications'][base_name+'Glass']=', '.join(apps)
    tmp=configfile.with_suffix('.tmp')
    with tmp.open('w') as f: cfg.write(f,space_around_delimiters=False)
    tmp.replace(configfile)
    if shutil.which('gdbus'):
        subprocess.run(['gdbus','emit','--session','--object-path','/KGlobalSettings','--signal','org.kde.KGlobalSettings.notifyChange','2','0'],capture_output=True,timeout=5)
    return True

def install_effect(opts):
    source=Path(__file__).resolve().parent/'opacity-template.js'
    metadata=source.with_name('effect-metadata.json')
    if not source.exists() or not metadata.exists():
        raise RuntimeError('Transparency template missing; reinstall wallpaper-style')
    opacity=opts.get('opacity',{})
    values={k:float(opacity.get(k,default)) for k,default in [('default',.8),('browser',.82),('launcher',.85)]}
    if any(not .2 <= v <= 1 for v in values.values()): raise ValueError('Opacity must be between 0.2 and 1')
    overrides=opts.get('app_overrides', {})
    if not isinstance(overrides, dict) or any(not isinstance(k,str) or not k.strip() or not isinstance(v,(int,float)) or not .2 <= v <= 1 for k,v in overrides.items()):
        raise ValueError('app_overrides must map app classes to opacity values between 0.2 and 1')
    script=source.read_text().replace('const defaultAmount = 0.80;',f"const defaultAmount = {values['default']};").replace('browser.test(cls) ? 0.82',f"browser.test(cls) ? {values['browser']}").replace('launchers.test(cls) ? 0.85',f"launchers.test(cls) ? {values['launcher']}")
    override_js = json.dumps(overrides, ensure_ascii=True)
    script=script.replace('    const key = "desktopScopedOpacityAnimation";', '    const userApps = '+override_js+';\n    function userAmount(cls) { const padded = " " + cls.toLowerCase() + " "; const key = Object.keys(userApps).find(name => padded.includes(" " + name.toLowerCase() + " ")); return key === undefined ? null : userApps[key]; }\n    const key = "desktopScopedOpacityAnimation";')
    script=script.replace('(app.test(String(window.windowClass)) || launchers.test(String(window.windowClass)))', '(app.test(String(window.windowClass)) || launchers.test(String(window.windowClass)) || userAmount(String(window.windowClass)) !== null)')
    script=script.replace('const amount = browser.test(cls) ?', 'const amount = userAmount(cls) !== null ? userAmount(cls) : browser.test(cls) ?')
    destination=Path.home()/'.local/share/kwin/effects/wallpaper-style'
    (destination/'contents/code').mkdir(parents=True,exist_ok=True)
    data=json.loads(metadata.read_text());data['KPlugin']['Id']='wallpaper-style';data['KPlugin']['Name']='Wallpaper Style transparency'
    (destination/'metadata.json').write_text(json.dumps(data,indent=2)+'\n')
    (destination/'contents/code/main.js').write_text(script)
    subprocess.run(['kwriteconfig6','--file','kwinrc','--group','Plugins','--key','wallpaper-styleEnabled','--type','bool','true'],check=True,timeout=5)
    # Replace our earlier personal effect only; do not interfere with other effects.
    subprocess.run(['kwriteconfig6','--file','kwinrc','--group','Plugins','--key','codex-glassEnabled','--type','bool','false'],check=True,timeout=5)
    for effect in ('codex-glass','wallpaper-style'):
        subprocess.run(['qdbus6','org.kde.KWin','/Effects','org.kde.kwin.Effects.unloadEffect',effect],capture_output=True,timeout=5)
    result=subprocess.run(['qdbus6','org.kde.KWin','/Effects','org.kde.kwin.Effects.loadEffect','wallpaper-style'],capture_output=True,text=True,timeout=5,check=True)
    if result.stdout.strip() != 'true': raise RuntimeError('KWin did not load the transparency effect')


def build_scheme(rgb):
    config = configparser.ConfigParser(interpolation=None,strict=False); config.optionxform = str
    source = Path.home()/'.config/kdeglobals'
    if not config.read(source): raise RuntimeError('Cannot read current KDE color scheme')
    target = configparser.ConfigParser(interpolation=None); target.optionxform = str
    accent = ','.join(map(str,rgb))
    hue,_,sat=colorsys.rgb_to_hls(*(v/255 for v in rgb))
    dark=','.join(str(round(v*255)) for v in colorsys.hls_to_rgb(hue,.075,min(sat,.20)))
    panel=','.join(str(round(v*255)) for v in colorsys.hls_to_rgb(hue,.13,min(sat,.18)))
    for section in config.sections():
        if section.startswith(('Colors:', 'ColorEffects:')):
            target[section] = dict(config[section])
            if section.startswith('Colors:'):
                if section in ('Colors:Window','Colors:View','Colors:Header','Colors:Tooltip','Colors:Button'):
                    target[section]['BackgroundNormal'] = panel if section in ('Colors:Window','Colors:Header','Colors:Button') else dark
                    target[section]['ForegroundNormal'] = '232,237,243'
                for key in ('DecorationFocus','DecorationHover','ForegroundLink','ForegroundVisited'):
                    if key in target[section]: target[section][key] = accent
    if 'Colors:Selection' in target:
        target['Colors:Selection']['BackgroundNormal'] = accent
        target['Colors:Selection']['BackgroundAlternate'] = accent
        target['Colors:Selection']['ForegroundNormal'] = '17,20,24'
    target['General'] = {'Name':f'Wallpaper Style #{rgb[0]:02x}{rgb[1]:02x}{rgb[2]:02x}'}
    folder = Path(os.environ.get('XDG_DATA_HOME',str(Path.home()/'.local/share')))/'color-schemes'; folder.mkdir(parents=True,exist_ok=True)
    # Plasma skips reapplying a scheme with the same name, even if its file
    # changed. A color-specific name makes wallpaper changes take effect.
    path = folder/f'WallpaperStyle-{rgb[0]:02x}{rgb[1]:02x}{rgb[2]:02x}.colors'
    temporary = path.with_suffix('.colors.tmp')
    with temporary.open('w') as f: target.write(f,space_around_delimiters=False)
    temporary.replace(path)
    return path

def apply(path=None):
    path = path or wallpaper(); rgb = extract(path); opts = settings(); scheme = build_scheme(rgb)
    make_kvantum(rgb, opts)
    subprocess.run(['plasma-apply-colorscheme',str(scheme)],check=True,timeout=20)
    print(f"Wallpaper: {path}\nAccent: #{rgb[0]:02x}{rgb[1]:02x}{rgb[2]:02x}",flush=True)
    return path

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--setup-effect',action='store_true',help='install or update desktop-aware transparency effect')
    parser.add_argument('--once',action='store_true',help='apply once and exit')
    parser.add_argument('--image',type=Path,help='apply colors from this local image once')
    parser.add_argument('--interval',type=int,default=10,help='poll interval in seconds (default: 10)')
    args=parser.parse_args()
    if args.interval < 2: parser.error('--interval must be at least 2 seconds')
    for executable in ('magick','plasma-apply-colorscheme') + (() if args.image else ('qdbus6',)):
        if not shutil.which(executable): parser.error(f'Missing dependency: {executable}')
    if args.setup_effect: install_effect(settings()); return
    if args.once or args.image: apply(args.image); return
    previous = None
    while True:
        try:
            current = wallpaper(); stat = current.stat(); signature=(str(current),stat.st_mtime_ns,stat.st_size)
            if signature != previous:
                apply(current); previous = signature
        except (OSError,ValueError,subprocess.SubprocessError,RuntimeError) as e:
            print(f'wallpaper-accent: {e}',file=sys.stderr,flush=True)
        time.sleep(args.interval)
if __name__ == '__main__': main()
