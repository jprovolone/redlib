// @license http://www.gnu.org/licenses/agpl-3.0.html AGPL-3.0
// Replaces native <select> menus with a menu matching the Feeds dropdown.
// The original <select> stays in the form (visually hidden) and remains the
// source of truth, so submitting, server rendering and no-JS all still work.
(function () {
    var openMenu = null;
    var uid = 0;

    var CHEVRON = '<svg class="ios-select-chevron" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m6 9 6 6 6-6"/></svg>';

    function labelFor(select) {
        if (select.getAttribute('aria-label')) return select.getAttribute('aria-label');
        if (select.title) return select.title;
        if (select.id) {
            var label = document.querySelector('label[for="' + CSS.escape(select.id) + '"]');
            if (label) return label.textContent.trim();
        }
        return 'Choose an option';
    }

    function close(restoreFocus) {
        if (!openMenu) return;
        var state = openMenu;
        openMenu = null;
        state.menu.hidden = true;
        state.wrapper.classList.remove('is-open');
        state.button.setAttribute('aria-expanded', 'false');
        state.button.removeAttribute('aria-activedescendant');
        if (restoreFocus) state.button.focus();
    }

    function enhance(select) {
        if (select.dataset.iosSelect || select.multiple || select.size > 1) return;
        select.dataset.iosSelect = 'true';
        var id = 'ios-select-' + (++uid);

        var wrapper = document.createElement('span');
        wrapper.className = 'ios-select';
        select.parentNode.insertBefore(wrapper, select);
        wrapper.appendChild(select);
        select.classList.add('ios-select-native');
        select.tabIndex = -1;
        select.setAttribute('aria-hidden', 'true');

        var button = document.createElement('button');
        button.type = 'button';
        button.className = 'ios-select-button';
        button.setAttribute('aria-haspopup', 'listbox');
        button.setAttribute('aria-expanded', 'false');
        button.setAttribute('aria-controls', id);
        var value = document.createElement('span');
        value.className = 'ios-select-value';
        button.appendChild(value);
        button.insertAdjacentHTML('beforeend', CHEVRON);
        wrapper.appendChild(button);

        var menu = document.createElement('ul');
        menu.className = 'ios-select-menu';
        menu.id = id;
        menu.setAttribute('role', 'listbox');
        menu.setAttribute('aria-label', labelFor(select));
        menu.hidden = true;
        wrapper.appendChild(menu);

        var items = [];
        var active = select.selectedIndex;

        Array.prototype.forEach.call(select.options, function (option, index) {
            var item = document.createElement('li');
            item.id = id + '-' + index;
            item.setAttribute('role', 'option');
            item.className = 'ios-select-option';
            item.textContent = option.textContent.trim();
            if (option.disabled) item.setAttribute('aria-disabled', 'true');
            item.addEventListener('click', function () { choose(index); });
            item.addEventListener('pointermove', function () { setActive(index); });
            menu.appendChild(item);
            items.push(item);
        });

        function sync() {
            var option = select.options[select.selectedIndex];
            value.textContent = option ? option.textContent.trim() : '';
            button.setAttribute('aria-label', labelFor(select) + ': ' + value.textContent);
            items.forEach(function (item, index) {
                item.setAttribute('aria-selected', index === select.selectedIndex ? 'true' : 'false');
            });
        }

        function setActive(index) {
            if (index < 0 || index >= items.length) return;
            active = index;
            items.forEach(function (item, i) { item.classList.toggle('is-active', i === index); });
            button.setAttribute('aria-activedescendant', items[index].id);
            items[index].scrollIntoView({ block: 'nearest' });
        }

        function choose(index) {
            if (select.options[index].disabled) return;
            var changed = select.selectedIndex !== index;
            select.selectedIndex = index;
            sync();
            close(true);
            if (changed) select.dispatchEvent(new Event('change', { bubbles: true }));
        }

        function position() {
            // Open upward when there isn't room below (e.g. above the tab bar),
            // and align to whichever side keeps the menu on screen.
            wrapper.classList.remove('opens-up', 'aligns-right');
            var rect = button.getBoundingClientRect();
            var tabs = document.querySelector('.app-tabs');
            var bottomLimit = tabs && tabs.offsetParent !== null ? tabs.getBoundingClientRect().top : window.innerHeight;
            var height = Math.min(menu.scrollHeight, window.innerHeight * 0.6);
            if (rect.bottom + height + 12 > bottomLimit && rect.top - height - 12 > 0) wrapper.classList.add('opens-up');
            if (rect.left + Math.max(menu.offsetWidth, 200) > window.innerWidth - 8) wrapper.classList.add('aligns-right');
        }

        function open() {
            if (openMenu && openMenu.menu === menu) return;
            close(false);
            openMenu = { menu: menu, wrapper: wrapper, button: button };
            menu.hidden = false;
            wrapper.classList.add('is-open');
            button.setAttribute('aria-expanded', 'true');
            position();
            setActive(Math.max(select.selectedIndex, 0));
        }

        button.addEventListener('click', function () {
            if (openMenu && openMenu.menu === menu) close(true); else open();
        });

        var typed = '';
        var typedTimer = 0;
        button.addEventListener('keydown', function (e) {
            var isOpen = openMenu && openMenu.menu === menu;
            switch (e.key) {
                case 'ArrowDown':
                case 'ArrowUp':
                    e.preventDefault();
                    if (!isOpen) { open(); return; }
                    setActive(active + (e.key === 'ArrowDown' ? 1 : -1));
                    return;
                case 'Home': if (isOpen) { e.preventDefault(); setActive(0); } return;
                case 'End': if (isOpen) { e.preventDefault(); setActive(items.length - 1); } return;
                case 'Enter':
                case ' ':
                    if (isOpen) { e.preventDefault(); choose(active); }
                    return;
                case 'Escape':
                    if (isOpen) { e.preventDefault(); close(true); }
                    return;
                case 'Tab':
                    if (isOpen) close(false);
                    return;
            }
            if (e.key.length === 1 && /\S/.test(e.key)) {
                // Type-ahead: jump to the first option starting with what was typed.
                clearTimeout(typedTimer);
                typed += e.key.toLowerCase();
                typedTimer = setTimeout(function () { typed = ''; }, 600);
                var match = items.findIndex(function (item) { return item.textContent.toLowerCase().indexOf(typed) === 0; });
                if (match >= 0) { if (!isOpen) open(); setActive(match); }
            }
        });

        // Keep in sync if something else changes the select (e.g. form reset).
        select.addEventListener('change', sync);
        if (select.form) select.form.addEventListener('reset', function () { setTimeout(sync); });
        sync();
    }

    document.addEventListener('click', function (e) {
        if (openMenu && !openMenu.wrapper.contains(e.target)) close(false);
    });
    window.addEventListener('resize', function () { close(false); });

    function register(root) {
        (root || document).querySelectorAll('.ios-ui select:not([data-native-select])').forEach(enhance);
    }

    window.redlibSelect = { register: register };
    register(document);
})();
// @license-end
