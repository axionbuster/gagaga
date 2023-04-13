// ChatGPT
// axionbuster

document.addEventListener('DOMContentLoaded', () => {
    const tableBody = document.getElementById('tableBody');

    // Find the extra path.
    let extraPath;
    {
        const url = window.location.pathname;
        const checkregex = /^\/user/; // URL must start with '/user'
        const where = url.search(checkregex);
        if (where !== -1) {
            // Find the extra path component that follows '/user'.
            const extra = url.substring(where + 5);
            extraPath = extra;
        } else {
            // Redirect (soft).
            window.location.pathname = '/user';
            // Unreachable.
            throw new Error('(gagaga) getLocation: Invalid path');
        }
    }

    const loadData = async (extraPath) => {
        const response = await fetch('/root' + extraPath);
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

        // TODO: Refactor
        for (const directory of directories) {
            addRowsToTable(directory);
        }

        for (const file of files) {
            addRowsToTable(file);
        }

        // Summarize the filesystem object and add it to the table.
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
            imgThumb.loading = 'lazy';
            imgThumb.style = 'background-image: url(/thumbimg);';
            // Remove imgThumb.style once loaded (attribute 'complete' is set)
            imgThumb.onload = () => {
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
