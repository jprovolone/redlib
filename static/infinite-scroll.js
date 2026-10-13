// @license http://www.gnu.org/licenses/agpl-3.0.html AGPL-3.0
// Endless scrolling. Follows the server-rendered "next" pagination link and
// appends the following page's posts. Without JS (or on error) the normal
// Next link remains, so this is a pure enhancement.
(function () {
    var list = document.querySelector('[data-infinite-list]');
    var nextLink = document.querySelector('main footer a[accesskey="N"]');
    if (!list || !nextLink || !('IntersectionObserver' in window) || !window.fetch || !window.DOMParser) return;

    var nextUrl = nextLink.href;
    var loading = false;
    var done = false;
    var seen = new Set();
    var MAX_PAGES = 40; // keep memory bounded on long sessions
    var pages = 1;

    function remember(root) {
        root.querySelectorAll('.post[id]').forEach(function (post) { seen.add(post.id); });
    }
    remember(list);

    var status = document.createElement('div');
    status.className = 'infinite-status';
    status.setAttribute('role', 'status');
    status.setAttribute('aria-live', 'polite');
    list.after(status);

    var sentinel = document.createElement('div');
    sentinel.className = 'infinite-sentinel';
    sentinel.setAttribute('aria-hidden', 'true');
    status.after(sentinel);

    nextLink.hidden = true;
    document.body.classList.add('infinite-enabled');

    function setStatus(kind, text) {
        status.textContent = '';
        status.hidden = !kind;
        if (kind === 'loading') {
            var spinner = document.createElement('span');
            spinner.className = 'infinite-spinner';
            status.appendChild(spinner);
            status.appendChild(document.createTextNode('Loading more posts…'));
        } else if (kind === 'error') {
            status.appendChild(document.createTextNode(text + ' '));
            var retry = document.createElement('button');
            retry.type = 'button';
            retry.className = 'infinite-retry';
            retry.textContent = 'Try again';
            retry.addEventListener('click', load);
            status.appendChild(retry);
        } else if (kind) {
            status.appendChild(document.createTextNode(text));
        }
    }
    setStatus(null);

    function finish(message) {
        done = true;
        observer.disconnect();
        setStatus('end', message);
    }

    function fallbackToLink(message) {
        // Restore the plain Next link so the user is never stuck.
        nextLink.href = nextUrl;
        nextLink.hidden = false;
        setStatus('error', message);
    }

    function enhance(nodes) {
        // Let the HLS loader and feed autoplay pick up newly inserted videos.
        nodes.forEach(function (node) {
            if (window.redlibSetupHls) window.redlibSetupHls(node);
        });
        if (window.redlibFeedVideo) window.redlibFeedVideo.register(list);
        if (window.redlibGallery) window.redlibGallery.register(list);
    }

    function load() {
        if (loading || done) return;
        if (pages >= MAX_PAGES) {
            nextLink.href = nextUrl;
            nextLink.hidden = false;
            finish('Reached the scroll limit for this page. Use Next to keep going.');
            return;
        }
        loading = true;
        setStatus('loading');
        fetch(nextUrl, { credentials: 'same-origin', headers: { Accept: 'text/html' } })
            .then(function (response) {
                if (!response.ok) throw new Error('HTTP ' + response.status);
                return response.text();
            })
            .then(function (html) {
                var doc = new DOMParser().parseFromString(html, 'text/html');
                var incoming = doc.querySelector('[data-infinite-list]');
                var following = doc.querySelector('main footer a[accesskey="N"]');
                var added = [];
                if (incoming) {
                    Array.prototype.slice.call(incoming.children).forEach(function (child) {
                        if (child.tagName === 'SCRIPT' || child.matches('hr.sep')) return;
                        var post = child.matches('.post[id]') ? child : child.querySelector('.post[id]');
                        if (post && seen.has(post.id)) return; // Reddit occasionally repeats posts across pages
                        var node = document.importNode(child, true);
                        list.appendChild(node);
                        remember(node);
                        added.push(node);
                    });
                }
                pages += 1;
                enhance(added);
                loading = false;
                if (following && following.getAttribute('href')) {
                    nextUrl = new URL(following.getAttribute('href'), nextUrl).href;
                    setStatus(null);
                    // If the new page didn't fill the viewport, keep going.
                    var rect = sentinel.getBoundingClientRect();
                    if (rect.top < window.innerHeight * 2) load();
                } else {
                    finish(added.length || pages > 2 ? "You're all caught up." : 'No more posts.');
                }
            })
            .catch(function () {
                loading = false;
                fallbackToLink("Couldn't load more posts.");
            });
    }

    var observer = new IntersectionObserver(function (entries) {
        if (entries.some(function (e) { return e.isIntersecting; })) load();
    }, { rootMargin: '0px 0px 1200px 0px' });
    observer.observe(sentinel);
})();
// @license-end
