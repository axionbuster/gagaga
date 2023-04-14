// ChatGPT
// axionbuster

document.addEventListener('DOMContentLoaded', () => {
    const tableBody = document.getElementById('tableBody');

    // Find the extra path.
    // If invalid, redirect to '/user'.
    let extraPath;
    {
        const url = window.location.pathname;
        const checkregex = /^\/user/; // URL must start with '/user'
        const where = url.search(checkregex);
        if (where !== -1) {
            // Find the extra path component that follows '/user'.
            const extra = url.substring(where + 5);
            extraPath = extra;

            // If empty, then '/'.
            if (extraPath === '') {
                extraPath = '/';
            }
        } else {
            // Redirect (soft).
            window.location.pathname = '/user';
            // Unreachable.
            throw new Error('(gagaga) getLocation: Invalid path');
        }
    }

    // Set the title and h1
    {
        const text = `File Server (${extraPath})`;
        document.title = text;
        const h1 = document.getElementsByTagName('h1')[0];
        h1.textContent = text;
    }

    // Disable the back (..) row if and only if we are at the root.
    if (extraPath === '/') {
        const backRow = document.getElementById('backRow');
        backRow.style = 'display: none;';
    } else {
        const backRow = document.getElementById('backRow');
        backRow.style = '';
    }

    // Do the same with the root (/) row button.
    if (extraPath === '/') {
        const rootRow = document.getElementById('rootRow');
        rootRow.style = 'display: none;';
    } else {
        const rootRow = document.getElementById('rootRow');
        rootRow.style = '';
    }

    // Inject the back row (..) link.
    {
        const backRow = document.getElementById('backRowLink');
        const parent = extraPath.substring(0, extraPath.lastIndexOf('/'));
        backRow.href = '/user' + parent;
    }

    const loadData = async (extraPath) => {
        const response = await fetch('/root' + extraPath);

        if (!response.ok) {
            // Let the user know that something went wrong
            // as a row in the table corresponding to the
            // HTTP status code and the status text.
            const tr = document.createElement('tr');
            const td = document.createElement('td');
            td.classList.add('error');
            td.colSpan = 3;
            td.textContent = `HTTP ${response.status}: ${response.statusText}`;
            tr.appendChild(td);
            tableBody.appendChild(tr);
            return;
        }

        const json = await response.json();

        // Check version. It is a three-digit number.
        // The first digit is the major version, and the second minor.
        // The third digit is the patch version.
        // So, check for major version 0. Incompatible with future versions.
        // Also: since we are in major 0, the minor version should be
        // checked, too.
        if (json.version[0] !== '0' || json.version[1] !== '1') {
            throw new Error(`Client supports 0.1.*. Server: (json.version = \"${json.version}\")
incompatible. Please update your client.`);
        }

        const files = json.files ? json.files : [];
        const directories = json.directories ? json.directories : [];

        // Sort by last modified date, from newest to oldest
        directories.sort((a, b) => new Date(b.last_modified) - new Date(a.last_modified));
        files.sort((a, b) => new Date(b.last_modified) - new Date(a.last_modified));

        // Add the rows to the table.
        for (const directory of directories) {
            addRowsToTable(directory);
        }
        for (const file of files) {
            addRowsToTable(file);
        }

        // Summarize a filesystem object and add it to the table.
        function addRowsToTable(fsObject) {
            // <tr>
            //  <td><img ... /></td>        (thumbnail)
            //  <td><a ...>...</a></td>     (name and link)
            //  <td>...</td>                (last modified)
            // </tr>

            const tr = document.createElement('tr');
            const td1 = document.createElement('td');
            const imgThumb = document.createElement('img');
            imgThumb.classList.add('thumb');
            imgThumb.src = fsObject.thumb_url;
            imgThumb.alt = ''; // thumbnail; decorative
            imgThumb.width = 100;
            imgThumb.height = 100;
            // Lazy load the thumbnail with your good old strategy,
            // background-image + loading=lazy + onload.
            // Remove CSS background-image (placeholder) once loaded.
            imgThumb.loading = 'lazy';
            imgThumb.style = 'background-image: url(/thumbimg);';
            imgThumb.onload = () => {
                // Remove imgThumb.style once loaded (attribute 'complete' is set).
                imgThumb.removeAttribute('style');
            };
            td1.appendChild(imgThumb);
            tr.appendChild(td1);
            const td2 = document.createElement('td');
            const a = document.createElement('a');
            a.href = fsObject.url;
            a.textContent = fsObject.name;
            td2.appendChild(a);
            tr.appendChild(td2);
            const td3 = document.createElement('td');
            td3.textContent = new Date(fsObject.last_modified).toLocaleString();
            tr.appendChild(td3);
            tableBody.appendChild(tr);
        }
    };

    loadData(extraPath);
});
