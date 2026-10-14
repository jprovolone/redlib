// @license http://www.gnu.org/licenses/agpl-3.0.html AGPL-3.0
// Fresh feeds, like the Reddit app: remembers which posts you've seen (only in
// this browser, for a few days) and, on hot feeds, moves seen posts down so new
// ones come first, with seen posts woven in between rather than hidden.
// Loaded synchronously right after the post list so reordering happens before
// the list is painted.
(function () {
    var list = document.querySelector('[data-infinite-list]');
    if (!list || !window.localStorage) return;

    var KEY = 'redlib:seen:v1';
    var MAX = 3000;                    // posts remembered
    var TTL = 4 * 24 * 60 * 60 * 1000; // forget after four days
    var UNSEEN_RUN = 3;                // unseen posts between each seen post
    var DWELL = 700;                   // ms a post must stay on screen to count as seen

    var store = {};
    try { store = JSON.parse(localStorage.getItem(KEY) || '{}') || {}; } catch (e) { store = {}; }
    (function prune() {
        var now = Date.now();
        var ids = Object.keys(store).filter(function (id) { return now - store[id] < TTL; });
        ids.sort(function (a, b) { return store[b] - store[a]; });
        var kept = {};
        ids.slice(0, MAX).forEach(function (id) { kept[id] = store[id]; });
        store = kept;
    })();

    var saveTimer = 0;
    function save() {
        saveTimer = 0;
        try { localStorage.setItem(KEY, JSON.stringify(store)); } catch (e) { /* storage full or disabled */ }
    }
    function markSeen(id) {
        if (store[id]) return;
        store[id] = Date.now();
        if (!saveTimer) saveTimer = setTimeout(save, 2000);
    }
    window.addEventListener('pagehide', function () { if (saveTimer) { clearTimeout(saveTimer); save(); } });

    function postOf(node) {
        if (node.nodeType !== 1) return null;
        return node.matches('.post[id]') ? node : node.querySelector('.post[id]');
    }

    // Reorders the given list children in place: pinned posts stay first, then
    // runs of unseen posts with one previously seen post after each run.
    var mutations = null;
    function arrange(nodes) {
        if (!list.hasAttribute('data-fresh')) return;
        var units = [];
        var pending = null;
        nodes.forEach(function (node) {
            if (node.nodeType === 1 && node.matches('hr.sep')) { pending = node; return; }
            var post = postOf(node);
            if (!post) return;
            units.push({ nodes: pending ? [pending, node] : [node], seen: !!store[post.id], pinned: post.classList.contains('stickied') });
            pending = null;
        });
        if (!units.some(function (u) { return u.seen; }) || units.every(function (u) { return u.seen; })) return;
        var last = units[units.length - 1].nodes;
        var anchor = last[last.length - 1].nextSibling;

        var ordered = [];
        while (units.length && units[0].pinned) ordered.push(units.shift());
        var unseen = units.filter(function (u) { return !u.seen; });
        var seen = units.filter(function (u) { return u.seen; });
        while (unseen.length || seen.length) {
            ordered = ordered.concat(unseen.splice(0, UNSEEN_RUN));
            if (seen.length) ordered.push(seen.shift());
            if (!unseen.length) { ordered = ordered.concat(seen); seen = []; }
        }

        var fragment = document.createDocumentFragment();
        ordered.forEach(function (u) { u.nodes.forEach(function (n) { fragment.appendChild(n); }); });
        list.insertBefore(fragment, anchor);
        // Our own moves must not be treated as a new batch.
        if (mutations) mutations.takeRecords();
    }

    // Seen tracking: a post counts once it has been mostly on screen for a moment.
    var timers = {};
    var observer = 'IntersectionObserver' in window ? new IntersectionObserver(function (entries) {
        entries.forEach(function (entry) {
            var id = entry.target.id;
            var visible = entry.intersectionRatio >= 0.6 || entry.intersectionRect.height >= window.innerHeight * 0.5;
            if (visible && !timers[id]) {
                timers[id] = setTimeout(function () { markSeen(id); observer.unobserve(entry.target); }, DWELL);
            } else if (!visible && timers[id]) {
                clearTimeout(timers[id]);
                delete timers[id];
            }
        });
    }, { threshold: [0, 0.25, 0.5, 0.6, 0.75, 1] }) : null;

    function track(nodes) {
        if (!observer) return;
        nodes.forEach(function (node) {
            var post = postOf(node);
            if (post && !store[post.id]) observer.observe(post);
        });
    }

    var initial = Array.prototype.slice.call(list.children);
    arrange(initial);
    track(initial);

    // Endless scrolling appends batches; arrange and track each one before it paints.
    if ('MutationObserver' in window) {
        mutations = new MutationObserver(function (records) {
            var added = [];
            records.forEach(function (r) { Array.prototype.forEach.call(r.addedNodes, function (n) { added.push(n); }); });
            if (!added.length) return;
            arrange(added);
            track(added);
        });
        mutations.observe(list, { childList: true });
    }
})();
// @license-end
