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

        files.sort((a, b) => new Date(b.last_modified) - new Date(a.last_modified));

        // FIXME: Turn this into safer HTML.
        const rows = files.map(file => `
            <tr>
                <td><img class="thumb" src="${file.thumb_url}" loading="lazy"></td>
                <td><a href="${file.url}">${file.name}</a></td>
                <td>${new Date(file.last_modified).toLocaleString()}</td>
            </tr>
        `).join('');

        tableBody.innerHTML = rows;
    };

    toggleTheme.addEventListener('click', () => {
        isDarkMode = !isDarkMode;
        document.body.classList.toggle('dark-mode', isDarkMode);
    });

    loadData();
});
