// Loaded only for this process and removed on exit. Other applications' data never leave KWin.
const ownerPid = @PID@;
const ownerBus = @BUS@;
const prefix = "stickitt-" + ownerPid + "-";
function owned(w) { return w.pid === ownerPid && w.resourceName.indexOf(prefix) === 0; }
function publish() {
    const snapshot = {
        current: workspace.currentDesktop.id,
        desktops: workspace.desktops.map(d => [d.id, d.name]),
        windows: workspace.windowList().filter(owned).map(w => ({
            name: w.resourceName,
            desktops: w.desktops.map(d => d.id)
        }))
    };
    callDBus(ownerBus, "/dev/stickitt/Windows", "dev.stickitt.Windows", "Update", JSON.stringify(snapshot));
}
function watch(w) {
    if (!owned(w)) return;
    w.desktopsChanged.connect(publish);
    publish();
}
workspace.windowAdded.connect(watch);
workspace.windowRemoved.connect(publish);
workspace.currentDesktopChanged.connect(publish);
const watchedDesktops = {};
function watchDesktops() {
    for (const d of workspace.desktops) {
        if (watchedDesktops[d.id]) continue;
        watchedDesktops[d.id] = true;
        d.nameChanged.connect(publish);
    }
    publish();
}
workspace.desktopsChanged.connect(watchDesktops);
watchDesktops();
for (const w of workspace.windowList()) watch(w);
publish();
