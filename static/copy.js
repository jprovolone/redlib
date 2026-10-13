async function copy() {
    var button = document.getElementById('copy');
    try {
        await navigator.clipboard.writeText(document.getElementById('bincode_str').value);
        button.textContent = 'Copied';
    } catch (e) {
        // Clipboard can be unavailable (e.g. non-HTTPS); select the code instead.
        document.getElementById('bincode_str').select();
        button.textContent = 'Select and copy';
    }
    setTimeout(function () { button.textContent = 'Copy'; }, 2000);
}

async function set_listener() {
    document.getElementById('copy').addEventListener('click', copy);
}

window.addEventListener('load', set_listener);
