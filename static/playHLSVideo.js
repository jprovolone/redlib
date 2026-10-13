// @license http://www.gnu.org/licenses/agpl-3.0.html AGPL-3.0
(function () {
    var configElement = document.getElementById('video_quality');
    var qualitySetting = configElement ? configElement.getAttribute('data-value') : 'best';
    var hlsAvailable = typeof Hls !== 'undefined' && Hls.isSupported();

    function getIndexOfDefault(length) {
        switch (qualitySetting) {
            case 'medium':
                return Math.floor(length / 2);
            case 'worst':
                return 0;
            default:
                return length - 1;
        }
    }

    function addQualitySelector(videoElement, hlsInstance, availableLevels) {
        var qualitySelector = document.createElement('select');
        qualitySelector.classList.add('quality-selector');
        var defaultIndex = getIndexOfDefault(availableLevels.length);
        availableLevels.forEach(function (level, index) {
            var option = document.createElement('option');
            option.value = index.toString();
            option.text = level.height + 'p (' + (level.bitrate / 1000).toFixed(0) + ' kbps)';
            if (index === defaultIndex) {
                option.selected = 'selected';
            }
            qualitySelector.appendChild(option);
        });
        qualitySelector.selectedIndex = defaultIndex;
        qualitySelector.addEventListener('change', function () {
            hlsInstance.nextLevel = qualitySelector.selectedIndex;
            hlsInstance.startLoad();
        });
        videoElement.parentNode.appendChild(qualitySelector);
    }

    function setup(source) {
        var playlist = source.src;
        var oldVideo = source.parentNode;
        var isFeed = oldVideo.hasAttribute('data-feed-video');
        // Feed videos are started by feed-video.js, never by this script.
        var autoplay = oldVideo.classList.contains('hls_autoplay') && !isFeed;

        if (oldVideo.hasAttribute('data-hls-initialized')) return;

        // Native HLS (e.g. Safari): don't use hls.js
        if (oldVideo.canPlayType(source.type) === 'probably') {
            oldVideo.setAttribute('data-hls-initialized', 'native');
            if (autoplay) {
                oldVideo.play().catch(function () {});
            }
            return;
        }

        if (!hlsAvailable) {
            // Fallback sources (mp4) remain on the element.
            if (autoplay) oldVideo.setAttribute('autoplay', '');
            return;
        }

        // cloneNode keeps all attributes (data-*, muted, controls, ...) of the video
        var newVideo = oldVideo.cloneNode(true);
        newVideo.querySelectorAll('source').forEach(function (s) { s.remove(); });
        newVideo.setAttribute('data-hls-initialized', 'pending');
        // Empty source to enable play event
        newVideo.src = 'about:blank';
        oldVideo.parentNode.replaceChild(newVideo, oldVideo);

        var wantPlay = false;
        var started = false;

        function playIfWanted() {
            if (wantPlay && !document.hidden && (!isFeed || newVideo.dataset.feedActive === 'true')) {
                var p = newVideo.play();
                if (p && p.catch) p.catch(function () {});
            }
        }

        function initializeHls() {
            if (started) return;
            started = true;
            newVideo.setAttribute('data-hls-initialized', 'true');
            var hls = new Hls({ autoStartLoad: false });
            hls.loadSource(playlist);
            hls.attachMedia(newVideo);
            hls.on(Hls.Events.MANIFEST_PARSED, function () {
                hls.loadLevel = getIndexOfDefault(hls.levels.length);
                var availableLevels = hls.levels.map(function (level) {
                    return { height: level.height, width: level.width, bitrate: level.bitrate };
                });
                addQualitySelector(newVideo, hls, availableLevels);
                hls.startLoad();
                // Skip if paused/offscreen/hidden in the meantime
                playIfWanted();
            });

            hls.on(Hls.Events.ERROR, function (event, data) {
                if (data.fatal) {
                    switch (data.type) {
                        case Hls.ErrorType.NETWORK_ERROR:
                            hls.startLoad();
                            break;
                        case Hls.ErrorType.MEDIA_ERROR:
                            hls.recoverMediaError();
                            break;
                        default:
                            hls.destroy();
                            break;
                    }
                }
                console.error('HLS error', data);
            });
        }

        newVideo.addEventListener('play', function () {
            wantPlay = true;
            initializeHls();
        });
        // The about:blank play may be rejected, so feed-video.js also signals activation.
        newVideo.addEventListener('feed-video-activate', function () {
            wantPlay = true;
            initializeHls();
        });
        newVideo.addEventListener('pause', function () {
            wantPlay = false;
        });
        newVideo.addEventListener('feed-video-deactivate', function () {
            wantPlay = false;
        });

        if (autoplay) {
            wantPlay = true;
            initializeHls();
        }
    }

    document
        .querySelectorAll("video source[type='application/vnd.apple.mpegurl']")
        .forEach(setup);
})();
// @license-end
