/* Keep the selected apps translucent only while visible on this desktop. */
(function () {
    "use strict";
    const defaultAmount = 0.80;
    const browser = /(?:^|\s)(?:brave-browser|brave|firedragon|FireDragon|crx_hnpfjngllnobngcgfapefoaidbinmjnm)(?:$|\s)/i;
    const app = /(?:^|\s)(?:(?:(?:com|org)\.openai\.)?(?:chatgpt|codex)|com\.anthropic\.Claude|claude-desktop|Claude|com\.microsoft\.VSCode|com\.visualstudio\.code|code|code-oss|vscode|Cursor|co\.anysphere\.cursor|garuda-toolbox|systemsettings|org\.kde\.systemsettings|systemsettings6|plasma-systemmonitor|org\.kde\.plasma-systemmonitor|firedragon|FireDragon|octopi|Octopi|garuda-system-maintenance|org\.garudalinux\.garuda-system-maintenance|Antigravity|com\.google\.Antigravity|brave-browser|Brave-browser|brave|docker-desktop|Docker Desktop|DockerDesktop|Heroic|heroic|com\.heroicgameslauncher\.hgl|crx_hnpfjngllnobngcgfapefoaidbinmjnm)(?:$|\s)/i;
    const launchers = /(?:^|\s)(?:VirtualBox Manager|VirtualBox|virtualbox|steam|Steam|steamwebhelper|Heroic|heroic|com\.heroicgameslauncher\.hgl)(?:$|\s)/i;
    const virtualMachine = /(?:VirtualBox Machine|VirtualBoxVM)/i;
    const key = "desktopScopedOpacityAnimation";
    function matches(window) {
        return window && (window.normalWindow || window.dialog) && (app.test(String(window.windowClass)) || launchers.test(String(window.windowClass)))
            && !virtualMachine.test(String(window.windowClass));
    }
    function clear(window) {
        if (window[key] !== undefined) {
            cancel(window[key]);
            window[key] = undefined;
        }
    }
    function sync(window) {
        if (!matches(window)) return;
        // A persistent animation otherwise makes KWin paint windows that the
        // desktop or minimize state would normally exclude from composition.
        if (!window.onCurrentDesktop || window.minimized || !window.visible) {
            clear(window);
            return;
        }
        if (window[key] === undefined) {
            const cls = String(window.windowClass);
            const amount = browser.test(cls) ? 0.82 : launchers.test(cls) ? 0.85 : defaultAmount;
            window[key] = set({
                window: window,
                duration: 1,
                keepAlive: false,
                animations: [{type: Effect.Opacity, from: amount, to: amount}]
            });
        }
    }
    function syncAll() {
        effects.stackingOrder.forEach(sync);
    }
    function manage(window) {
        if (!matches(window)) return;
        window.windowDesktopsChanged.connect(function () { sync(window); });
        window.minimizedChanged.connect(function () { sync(window); });
        sync(window);
    }
    effects.windowAdded.connect(manage);
    effects.windowClosed.connect(function(window) { if (matches(window)) clear(window); });
    effects.desktopChanged.connect(syncAll);
    effects.desktopChangingCancelled.connect(syncAll);
    effects.stackingOrder.forEach(manage);
    print("APP_GLASS loaded (Brave/FireDragon/WhatsApp 82%; VirtualBox/Steam/Heroic 85%; other selected apps 80%; desktop-aware)");
    print("APP_GLASS_SCOPE " + JSON.stringify(effects.stackingOrder.filter(matches).map(function(window) {
        return {app: String(window.windowClass), currentDesktop: window.onCurrentDesktop,
            minimized: window.minimized, managed: window[key] !== undefined};
    })));
})();
