// ChatGPT
// axionbuster

// Format Date like "just now", "a minute ago",
// "2 hours ago", "yesterday", "a week ago", "2 weeks ago",
// "a month ago", "2 months ago", "a year ago", "2 years ago", "Jan 1, 2021", etc.
// Assume US English.
// Strategy "A"
function shortFormatDateA_en_US(date) {
    const now = new Date();
    const diffInSeconds = Math.floor((now - date) / 1000);
    const diffInMinutes = Math.floor(diffInSeconds / 60);
    const diffInHours = Math.floor(diffInMinutes / 60);
    const diffInDays = Math.floor(diffInHours / 24);
    const diffInWeeks = Math.floor(diffInDays / 7);

    if (diffInSeconds < 600) {
        return "just now";
    } else if (diffInMinutes < 60) {
        if (diffInMinutes === 1) {
            return "a minute ago";
        }
        return `${diffInMinutes} minutes ago`;
    } else if (diffInHours < 12) {
        if (diffInHours === 1) {
            return "an hour ago";
        }
        return `${diffInHours} hours ago`;
    } else if (diffInHours >= 12 && diffInHours < 24 && date.getDate() === now.getDate()) {
        return "today";
    } else if (diffInHours >= 12 && diffInHours < 24 && date.getDate() !== now.getDate()) {
        // Yesterday and less than 24 hours ago.
        return "yesterday";
    } else if (diffInDays <= 30) {
        if (diffInWeeks <= 1) {
            if (diffInDays === 1) {
                // Yesterday and 24 hours ago or later.
                return "yesterday";
            }
            return `${diffInDays} days ago`;
        }
        return `${diffInWeeks} weeks ago`;
    } else {
        const options = { year: 'numeric', month: 'short', day: 'numeric' };
        return date.toLocaleDateString('en-US', options);
    }
}

// Summarize a filesystem object and add it to the table.
function showEntry(fsObject) {
    // <li>
    //     <div class="file">
    //         <a class="thumb" href="{url}">
    //              <img src="{thumb_url}" alt="" width="32" height="32">
    //         </a>
    //         <div class="info">
    //             <a class="filename" href="{url}">{name}</a>
    //             <div class="byline">{last_modified}</div>
    //         </div>
    //     </div>
    // </li>

    const { name, last_modified, url, thumb_url } = fsObject;
    const showLastModified = shortFormatDateA_en_US(new Date(last_modified));

    const li = document.createElement("li");
    const file = document.createElement("div");
    file.classList.add("file");
    const thumb = document.createElement("a");
    thumb.classList.add("thumb");
    thumb.href = url;
    const img = document.createElement("img");
    img.src = thumb_url;
    img.alt = "";
    img.width = 32;
    img.height = 32;
    thumb.appendChild(img);
    file.appendChild(thumb);
    const info = document.createElement("div");
    info.classList.add("info");
    const filename = document.createElement("a");
    filename.classList.add("filename");
    filename.href = url;
    filename.textContent = name;
    info.appendChild(filename);
    const byline = document.createElement("div");
    byline.classList.add("byline");
    byline.textContent = showLastModified;
    info.appendChild(byline);
    file.appendChild(info);
    li.appendChild(file);

    // Wow, I really don't want to do this.
    // See, this is why there are such things as frameworks.
    // This is messed up.
    return li;
}

// Create a new file item, representing the root (/) directory.
function showRoot(uList) {
    // <li>
    //     <div class="file">
    //         <a class="thumb" href="/user">
    //              <img src="/thumbdir" alt="" width="32" height="32">
    //         </a>
    //         <div class="info">
    //             <a class="filename" href="/user">/</a>
    //         </div>
    //     </div>
    // </li>

    const li = document.createElement("li");
    const file = document.createElement("div");
    file.classList.add("file");
    const thumb = document.createElement("a");
    thumb.classList.add("thumb");
    thumb.href = "/user";
    const img = document.createElement("img");
    img.src = "/thumbdir";
    img.alt = "";
    img.width = 32;
    img.height = 32;
    thumb.appendChild(img);
    file.appendChild(thumb);
    const info = document.createElement("div");
    info.classList.add("info");
    const filename = document.createElement("a");
    filename.classList.add("filename");
    filename.href = "/user";
    filename.textContent = "/";
    info.appendChild(filename);
    file.appendChild(info);
    li.appendChild(file);

    return li;
}

// And, I need to do that all over again. No!
function showParent(parentUrl) {
    // <li>
    //     <div class="file">
    //         <a class="thumb" href="{parentUrl}">
    //              <img src="/thumbdir" alt="" width="32" height="32">
    //         </a>
    //         <div class="info">
    //             <a class="filename" href="/user">..</a>
    //         </div>
    //     </div>
    // </li>

    const li = document.createElement("li");
    const file = document.createElement("div");
    file.classList.add("file");
    const thumb = document.createElement("a");
    thumb.classList.add("thumb");
    thumb.href = parentUrl;
    const img = document.createElement("img");
    img.src = "/thumbdir";
    img.alt = "";
    img.width = 32;
    img.height = 32;
    thumb.appendChild(img);
    file.appendChild(thumb);
    const info = document.createElement("div");
    info.classList.add("info");
    const filename = document.createElement("a");
    filename.classList.add("filename");
    filename.href = parentUrl;
    filename.textContent = "..";
    info.appendChild(filename);
    file.appendChild(info);
    li.appendChild(file);

    return li;
}

// Something went wrong as an item
function showError(errorString) {
    // <li>{errorString}</li>

    const li = document.createElement("li");
    li.textContent = errorString;

    return li;
}

// That was a lot of work.
// Later, I'll refactor that when I choose a framework.
// But for now, I'm just going to get this working.

document.addEventListener('DOMContentLoaded', () => {
    const listFile = document.getElementById("listfile");

    // Create root and parent links.
    const rootLink = showRoot(listFile);
    listFile.appendChild(rootLink);
    const parentLink = showParent(listFile, "/user");
    listFile.appendChild(parentLink);

    // Find the extra path.
    // If invalid, redirect to "/user"
    let extraPath;
    {
        const url = window.location.pathname;
        const checkregex = /^\/user/; // URL must start with "/user"
        const where = url.search(checkregex);
        if (where !== -1) {
            // Find the extra path component that follows "/user".
            const extra = url.substring(where + 5);
            extraPath = extra;

            // If empty, then "/".
            if (extraPath === "") {
                extraPath = "/";
            }
        } else {
            // Redirect (soft).
            window.location.pathname = "/user";
            // Unreachable.
            throw new Error(`(gagaga) getLocation: Invalid path ${url}`);
        }
    }

    // Set the title and h1
    {
        const text = `File Server (${extraPath})`;
        document.title = text;
        const h1 = document.getElementsByTagName("h1")[0];
        h1.textContent = text;
    }

    // Disable the back (..) link if and only if we are at the root.
    // FIXME.
    if (extraPath === '/') {
        parentLink.classList.add("hide");
    } else {
        parentLink.classList.remove("hide");
    }

    // Do the same with the root (/) row button.
    if (extraPath === '/') {
        rootLink.classList.add("hide");
    } else {
        rootLink.classList.remove("hide");
    }

    // Inject the back row (..) link.
    {
        const parent = extraPath.substring(0, extraPath.lastIndexOf('/'));
        const thumb = parentLink.getElementsByClassName("thumb")[0];
        thumb.href = "/user" + parent;
        const filename = parentLink.getElementsByClassName("filename")[0];
        filename.href = "/user" + parent;
    }

    const loadAndShow = async (extraPath) => {
        const response = await fetch('/root' + extraPath);

        if (!response.ok) {
            const li = showError(`Error: ${response.status} ${response.statusText}`);
            listFile.appendChild(li);
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
            let li = showEntry(directory);
            listFile.appendChild(li);
        }
        for (const file of files) {
            let li = showEntry(file);
            listFile.appendChild(li);
        }
    };

    loadAndShow(extraPath);
});
