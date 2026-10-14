// @license http://www.gnu.org/licenses/agpl-3.0.html AGPL-3.0
// iOS-style pull to refresh: at the top of the page, pulling down stretches the
// content with rubber-band resistance; releasing past the threshold reloads.
// Touch devices only; pages with editable forms are left alone so a stray pull
// never discards unsaved input.
(function () {
    if (!('ontouchstart' in window) || !window.requestAnimationFrame) return;
    var main = document.querySelector('body > main');
    if (!main || document.querySelector('textarea, .prefs')) return;

    var THRESHOLD = 70;   // pull distance (px) that triggers a refresh
    var MAX = 130;        // asymptotic limit of the rubber band
    var HOLD = 56;        // resting offset while refreshing
    var reduceMotion = window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;

    var indicator = document.createElement('div');
    indicator.className = 'ptr-indicator';
    indicator.setAttribute('role', 'status');
    indicator.setAttribute('aria-live', 'polite');
    indicator.innerHTML = '<span class="ptr-spinner" aria-hidden="true"><svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round"><path d="M12 5v14m-6-6 6 6 6-6"/></svg></span><span class="ptr-label"></span>';
    main.before(indicator);
    document.documentElement.classList.add('ptr-enabled');

    var label = indicator.querySelector('.ptr-label');
    var startX = 0, startY = null, pull = 0, active = false, refreshing = false, frame = 0;

    // Resistance grows with distance, like UIScrollView's rubber band.
    function damp(distance) {
        return MAX * (1 - 1 / (distance * 0.55 / MAX + 1));
    }

    function render() {
        frame = 0;
        main.style.transform = pull ? 'translate3d(0,' + pull + 'px,0)' : '';
        indicator.style.transform = 'translate3d(-50%,' + Math.max(pull - 40, -40) / 2 + 'px,0)';
        indicator.style.opacity = String(Math.min(1, pull / (THRESHOLD * 0.8)));
        indicator.classList.toggle('is-ready', pull >= THRESHOLD);
    }

    function schedule() {
        if (!frame) frame = requestAnimationFrame(render);
    }

    function settle(to, done) {
        if (frame) { cancelAnimationFrame(frame); frame = 0; }
        if (reduceMotion) { pull = to; render(); if (done) done(); return; }
        main.classList.add('ptr-settling');
        indicator.classList.add('ptr-settling');
        pull = to;
        render();
        setTimeout(function () {
            main.classList.remove('ptr-settling');
            indicator.classList.remove('ptr-settling');
            if (done) done();
        }, 420);
    }

    function atTop() {
        return (window.scrollY || document.documentElement.scrollTop || 0) <= 0;
    }

    document.addEventListener('touchstart', function (event) {
        startY = null;
        if (refreshing || event.touches.length !== 1 || !atTop()) return;
        // Leave horizontal swipers, open menus, media and form controls alone.
        if (event.target.closest && event.target.closest('.feed-gallery-track, details[open], .ios-select, video, input, select, button')) return;
        startX = event.touches[0].clientX;
        startY = event.touches[0].clientY;
        active = false;
    }, { passive: true });

    document.addEventListener('touchmove', function (event) {
        if (startY === null) return;
        var dx = event.touches[0].clientX - startX;
        var dy = event.touches[0].clientY - startY;
        if (!active) {
            // Wait for a clear downward intent; a sideways or upward start is a normal gesture.
            if (dy < 0 || Math.abs(dx) > Math.abs(dy)) { if (Math.abs(dx) > 8 || dy < -4) startY = null; return; }
            if (dy < 4) return;
            if (!atTop()) { startY = null; return; }
            active = true;
        }
        if (event.cancelable) event.preventDefault();
        pull = damp(Math.max(0, dy));
        label.textContent = pull >= THRESHOLD ? 'Release to refresh' : '';
        schedule();
    }, { passive: false });

    function end() {
        if (startY === null) return;
        startY = null;
        if (!active) return;
        active = false;
        if (pull >= THRESHOLD) {
            refreshing = true;
            indicator.classList.add('is-refreshing');
            label.textContent = 'Refreshing…';
            settle(HOLD, function () {
                // Give the spinner a beat to register before the page goes away.
                location.reload();
            });
        } else {
            label.textContent = '';
            settle(0);
        }
    }
    document.addEventListener('touchend', end, { passive: true });
    document.addEventListener('touchcancel', end, { passive: true });

    // Returning via back/forward cache must not show a stuck spinner.
    window.addEventListener('pageshow', function (event) {
        if (!event.persisted) return;
        refreshing = false;
        indicator.classList.remove('is-refreshing', 'is-ready');
        label.textContent = '';
        pull = 0;
        render();
    });
})();
// @license-end
