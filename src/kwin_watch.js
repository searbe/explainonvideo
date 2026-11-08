// Loaded into KWin at run time by kwin.rs (never installed). It does two things:
// - tells the app where every window is (and their stacking order), whenever that changes, so it can offer them
//   for choosing and follow the ones being recorded;
// - makes the app's own floating windows (role eov-*) stay on top, out of the task bar, on every desktop, and out
//   of screen recordings, so the controls never appear in a video.
var PID = __PID__;
var BUS = "__BUS__";
var watched = {};

function geometry(w) {
    var g = w.frameGeometry;
    return [Math.round(g.x), Math.round(g.y), Math.round(g.width), Math.round(g.height)];
}

function shown(w) {
    if (w.minimized || w.deleted || w.pid === PID) return false;
    if (!(w.normalWindow || w.dialog || w.utility)) return false;
    if (w.onAllDesktops) return true;
    var ds = w.desktops || [];
    for (var i = 0; i < ds.length; i++) if (ds[i] === workspace.currentDesktop) return true;
    return ds.length === 0;
}

// A menu, dropdown, tooltip or other popup: not offered for choosing, but recorded with the window it belongs to.
function popup(w) {
    return !w.deleted && w.pid !== PID && !!(w.popupWindow || w.dropdownMenu || w.popupMenu || w.comboBox
                                             || w.tooltip || w.menu);
}

function report() {
    var out = [];
    var order = workspace.stackingOrder;
    for (var i = 0; i < order.length; i++) {
        var w = order[i];
        var isPopup = !shown(w);
        if (isPopup && !popup(w)) continue;
        var parent = "";
        try { if (w.transientFor) parent = String(w.transientFor.internalId); } catch (e) {}
        out.push({id: String(w.internalId), title: String(w.caption), app: String(w.resourceClass),
                  rect: geometry(w), popup: isPopup, parent: parent, pid: w.pid || 0});
    }
    var screens = workspace.screens.map(function (s) {
        return {rect: [s.geometry.x, s.geometry.y, s.geometry.width, s.geometry.height]};
    });
    callDBus(BUS, "/", "local.explainonvideo", "Windows", JSON.stringify({windows: out, screens: screens}));
}

function style(w) {
    var role = String(w.windowRole || "");
    if (w.pid !== PID || role.indexOf("eov-") !== 0) return;
    w.keepAbove = true;
    w.skipTaskbar = true;
    w.skipPager = true;
    w.skipSwitcher = true;
    w.onAllDesktops = true;
    if (!__VISIBLE__) { try { w.excludeFromCapture = true; } catch (e) {} }  // (Plasma 6.4 and later)
}

function watch(w) {
    if (watched[w.internalId]) return;
    watched[w.internalId] = true;
    style(w);
    w.frameGeometryChanged.connect(report);
    w.minimizedChanged.connect(report);
    try { w.desktopsChanged.connect(report); } catch (e) {}
    try { w.windowRoleChanged.connect(function () { style(w); }); } catch (e) {}
    try { w.captionChanged.connect(report); } catch (e) {}
}

workspace.windowList().forEach(watch);
workspace.windowAdded.connect(function (w) { watch(w); report(); });
workspace.windowRemoved.connect(function (w) { delete watched[w.internalId]; report(); });
workspace.currentDesktopChanged.connect(report);
try { workspace.stackingOrderChanged.connect(report); } catch (e) {}
try { workspace.screensChanged.connect(report); } catch (e) {}
report();
