// @license http://www.gnu.org/licenses/agpl-3.0.html AGPL-3.0
// Rates a post's community without leaving the page. Without JS (or on error)
// the form submits normally and the server redirects back to the post.
(function () {
    if (!window.fetch || !window.FormData || !window.URLSearchParams) return;

    function apply(community, rating) {
        document.querySelectorAll('.post-rate').forEach(function (menu) {
            if (menu.getAttribute('data-community') !== community) return;
            menu.setAttribute('data-rating', rating);
            menu.querySelectorAll('button[name="rating"]').forEach(function (button) {
                if (button.value !== 'clear') button.setAttribute('aria-pressed', String(button.value === rating));
            });
        });
    }

    document.addEventListener('submit', function (event) {
        var form = event.target;
        if (!form.matches || !form.matches('.post-rate-menu') || form.getAttribute('data-native')) return;
        var submitter = event.submitter;
        if (!submitter || submitter.name !== 'rating') return;
        event.preventDefault();
        var data = new FormData(form);
        data.append('rating', submitter.value);
        var menu = form.closest('.post-rate');
        var community = menu.getAttribute('data-community');
        var previous = menu.getAttribute('data-rating');
        var rating = submitter.value === 'clear' ? '' : submitter.value;
        apply(community, rating);
        menu.open = false;
        fetch(form.action, {
            method: 'POST',
            credentials: 'same-origin',
            redirect: 'manual',
            body: new URLSearchParams(data)
        }).then(function (response) {
            // A redirect means the rating was saved.
            if (response.type !== 'opaqueredirect' && !response.ok) throw new Error('HTTP ' + response.status);
        }).catch(function () {
            // Show the server's explanation with a normal submission.
            apply(community, previous);
            form.setAttribute('data-native', '1');
            if (form.requestSubmit) form.requestSubmit(submitter); else form.submit();
        });
    });

    // Close an open menu when clicking elsewhere or pressing Escape.
    document.addEventListener('click', function (event) {
        document.querySelectorAll('.post-rate[open]').forEach(function (menu) {
            if (!menu.contains(event.target)) menu.open = false;
        });
    });
    document.addEventListener('keydown', function (event) {
        if (event.key !== 'Escape') return;
        document.querySelectorAll('.post-rate[open]').forEach(function (menu) {
            menu.open = false;
            menu.querySelector('summary').focus();
        });
    });
})();
// @license-end
