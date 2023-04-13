// ChatGPT
// axionbuster

document.addEventListener('DOMContentLoaded', () => {
    const tableBody = document.getElementById('tableBody');
    const toggleTheme = document.getElementById('toggleTheme');

    let isDarkMode = false;

    const loadData = async () => {
        const response = await fetch('/root/');
        const json = await response.json();
        const files = json.files;
        const directories = json.directories;

        // Sort by last modified date, from newest to oldest
        directories.sort((a, b) => new Date(b.last_modified) - new Date(a.last_modified));
        files.sort((a, b) => new Date(b.last_modified) - new Date(a.last_modified));

        for (const directory of directories) {
            // <tr>
            //  <td><img ... /></td>        (thumbnail)
            //  <td><a ...>...</a></td>     (name and link)
            //  <td>...</td>                (last modified)
            // </tr>

            const tr = document.createElement('tr');
            const td1 = document.createElement('td');
            const imgThumb = document.createElement('img');
            imgThumb.classList.add('thumb');
            imgThumb.src = directory.thumb_url;
            imgThumb.alt = ''; // thumbnail; decorative
            imgThumb.width = 100;
            imgThumb.height = 100;
            imgThumb.loading = 'lazy';
            imgThumb.style = 'background-image: url(/thumbimg);'
            // Remove imgThumb.style once loaded (attribute 'complete' is set)
            imgThumb.onload = () => {
                imgThumb.removeAttribute('style');
            };
            td1.appendChild(imgThumb);
            tr.appendChild(td1);
            const td2 = document.createElement('td');
            const a = document.createElement('a');
            a.href = directory.url;
            a.textContent = directory.name;
            td2.appendChild(a);
            tr.appendChild(td2);
            const td3 = document.createElement('td');
            td3.textContent = new Date(directory.last_modified).toLocaleString();
            tr.appendChild(td3);
            tableBody.appendChild(tr);
        }

        for (const file of files) {
            // <tr>
            //  <td><img ... /></td>        (thumbnail)
            //  <td><a ...>...</a></td>     (name and link)
            //  <td>...</td>                (last modified)
            // </tr>

            const tr = document.createElement('tr');
            const td1 = document.createElement('td');
            const imgThumb = document.createElement('img');
            imgThumb.classList.add('thumb');
            imgThumb.src = file.thumb_url;
            imgThumb.alt = ''; // thumbnail; decorative
            imgThumb.width = 100;
            imgThumb.height = 100;
            imgThumb.loading = 'lazy';
            imgThumb.style = 'background-image: url(/thumbimg);'
            // Remove imgThumb.style once loaded (attribute 'complete' is set)
            imgThumb.onload = () => {
                imgThumb.removeAttribute('style');
            };
            td1.appendChild(imgThumb);
            tr.appendChild(td1);
            const td2 = document.createElement('td');
            const a = document.createElement('a');
            a.href = file.url;
            a.textContent = file.name;
            td2.appendChild(a);
            tr.appendChild(td2);
            const td3 = document.createElement('td');
            td3.textContent = new Date(file.last_modified).toLocaleString();
            tr.appendChild(td3);
            tableBody.appendChild(tr);
        }
    };

    toggleTheme.addEventListener('click', () => {
        isDarkMode = !isDarkMode;
        document.body.classList.toggle('dark-mode', isDarkMode);
    });

    loadData();
});
