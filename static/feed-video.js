// @license http://www.gnu.org/licenses/agpl-3.0.html AGPL-3.0
// Muted in-feed autoplay: the most visible video (>=50%) plays, others pause.
// Exposes window.redlibFeedVideo.register(root) so endless scrolling can add
// videos that arrive after page load.
(function () {
    var videos = [];
    var connection = navigator.connection || {};
    var motionQuery = window.matchMedia ? window.matchMedia('(prefers-reduced-motion: reduce)') : null;
    var autoplayAllowed = !(motionQuery && motionQuery.matches) && !connection.saveData;
    var ratios = new Map();
    var userPaused = new Set();
    var expectedPause = new Set();
    var observer = null;

    function safePlay(video) {
        try {
            var p = video.play();
            if (p && p.catch) p.catch(function () {});
        } catch (e) {}
    }

    function pauseByScript(video) {
        if (video.paused) return;
        expectedPause.add(video);
        try { video.pause(); } catch (e) { expectedPause.delete(video); }
    }

    function eligible(video) {
        return autoplayAllowed
            && video.dataset.autoplay !== 'off'
            && !document.hidden
            && !video.closest('.post_blurred')
            && !userPaused.has(video)
            && (ratios.get(video) || 0) >= 0.5;
    }

    function update() {
        var best = null;
        var bestRatio = 0;
        videos.forEach(function (video) {
            var ratio = ratios.get(video) || 0;
            if (eligible(video) && ratio > bestRatio) {
                best = video;
                bestRatio = ratio;
            }
        });
        videos.forEach(function (video) {
            if (video === best) {
                video.dataset.feedActive = 'true';
                if (video.paused) {
                    // Lets playHLSVideo.js lazily attach hls.js; plain videos just play.
                    video.dispatchEvent(new CustomEvent('feed-video-activate'));
                    safePlay(video);
                }
            } else if (video.dataset.feedActive || !video.paused) {
                delete video.dataset.feedActive;
                video.dispatchEvent(new CustomEvent('feed-video-deactivate'));
                pauseByScript(video);
            }
        });
    }

    function syncToggle(video) {
        var wrapper = video.closest('.feed-video');
        var button = wrapper && wrapper.querySelector('.video-sound-toggle');
        if (!button) return;
        button.setAttribute('aria-pressed', video.muted ? 'false' : 'true');
        button.setAttribute('aria-label', video.muted ? 'Unmute video' : 'Mute video');
    }

    function setup(video) {
        video.dataset.feedRegistered = 'true';
        videos.push(video);
        video.defaultMuted = true;
        video.muted = true;
        var wrapper = video.closest('.feed-video');
        var button = wrapper && wrapper.querySelector('.video-sound-toggle');

        if (button) {
            button.hidden = false;
            syncToggle(video);
            button.addEventListener('click', function () {
                video.muted = !video.muted;
                if (!video.muted) {
                    video.dispatchEvent(new CustomEvent('feed-video-activate'));
                    safePlay(video);
                }
            });
        }

        video.addEventListener('volumechange', function () {
            if (!video.muted) {
                // Only one video may have sound at a time; never unmute others automatically.
                videos.forEach(function (other) {
                    if (other !== video && !other.muted) other.muted = true;
                });
            }
            syncToggle(video);
        });

        video.addEventListener('pause', function () {
            if (expectedPause.delete(video)) return;
            if (!video.ended && !document.hidden) userPaused.add(video);
        });
        video.addEventListener('play', function () {
            userPaused.delete(video);
        });

        if (observer) observer.observe(video);
    }

    if ('IntersectionObserver' in window) {
        observer = new IntersectionObserver(function (entries) {
            entries.forEach(function (entry) {
                ratios.set(entry.target, entry.isIntersecting ? entry.intersectionRatio : 0);
                if (entry.intersectionRatio < 0.5) userPaused.delete(entry.target);
            });
            update();
        }, { threshold: [0, 0.25, 0.5, 0.75, 1] });
    }

    function register(root) {
        (root || document).querySelectorAll('video[data-feed-video]:not([data-feed-registered])').forEach(setup);
    }

    window.redlibFeedVideo = { register: register };
    register(document);
    document.addEventListener('visibilitychange', update);
})();
// @license-end
