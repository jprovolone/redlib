// @license http://www.gnu.org/licenses/agpl-3.0.html AGPL-3.0
// Swipeable in-feed galleries. Swiping and scrolling are native CSS scroll-snap;
// this only keeps the counter and dots in sync and adds arrow buttons/keys.
// Exposes window.redlibGallery.register(root) for endless scrolling.
(function () {
    function setup(gallery) {
        gallery.dataset.galleryReady = 'true';
        var track = gallery.querySelector('.feed-gallery-track');
        var slides = track ? track.querySelectorAll('.feed-gallery-slide') : [];
        if (slides.length < 2) return;
        var count = gallery.querySelector('.feed-gallery-count');
        var dots = gallery.querySelectorAll('.feed-gallery-dots span');
        var prev = gallery.querySelector('.feed-gallery-nav.prev');
        var next = gallery.querySelector('.feed-gallery-nav.next');
        var current = 0;

        function index() {
            return Math.round(track.scrollLeft / Math.max(track.clientWidth, 1));
        }

        function render() {
            var i = Math.min(Math.max(index(), 0), slides.length - 1);
            if (i === current && count.dataset.rendered) return;
            current = i;
            count.dataset.rendered = 'true';
            count.textContent = (i + 1) + '/' + slides.length;
            dots.forEach(function (dot, d) { dot.classList.toggle('active', d === i); });
            if (prev) prev.disabled = i === 0;
            if (next) next.disabled = i === slides.length - 1;
            track.setAttribute('aria-label', 'Gallery, image ' + (i + 1) + ' of ' + slides.length);
        }

        function go(i) {
            i = Math.min(Math.max(i, 0), slides.length - 1);
            var reduce = window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;
            track.scrollTo({ left: i * track.clientWidth, behavior: reduce ? 'auto' : 'smooth' });
        }

        var frame = 0;
        track.addEventListener('scroll', function () {
            if (frame) return;
            frame = requestAnimationFrame(function () { frame = 0; render(); });
        }, { passive: true });

        if (prev) { prev.hidden = false; prev.addEventListener('click', function () { go(current - 1); }); }
        if (next) { next.hidden = false; next.addEventListener('click', function () { go(current + 1); }); }

        track.addEventListener('keydown', function (e) {
            if (e.key === 'ArrowRight') { e.preventDefault(); go(current + 1); }
            if (e.key === 'ArrowLeft') { e.preventDefault(); go(current - 1); }
        });

        // Keep position after rotation/resizing.
        window.addEventListener('resize', function () { track.scrollLeft = current * track.clientWidth; });
        render();
    }

    function register(root) {
        (root || document).querySelectorAll('[data-gallery]:not([data-gallery-ready])').forEach(setup);
    }

    window.redlibGallery = { register: register };
    register(document);
})();
// @license-end
