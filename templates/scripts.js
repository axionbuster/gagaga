// ChatGPT
// axionbuster

// Origin of the listing service. To be filled by a templating engine.
const listOrigin = "<%= list_origin %>";

// Origin of the thumbnail service. Same, to be filled by a templating engine.
const thumbOrigin = "<%= thumb_origin %>";

// Format Date like "just now", "a minute ago",
// "2 hours ago", "yesterday", "a week ago", "2 weeks ago",
// "a month ago", "2 months ago", "a year ago", "2 years ago", "Jan 1, 2021", etc.
// Assume US English.
// Strategy "A"
function shortFormatDateA_en_US(dateInput) {
    // Guard against some typing errors.
    // I should use TypeScript, though.
    if ((typeof dateInput === "undefined") || (dateInput === null)) {
        throw new Error("date is undefined or null");
    }
    if (!(dateInput instanceof Date) && !(typeof dateInput === "string")) {
        throw new Error("date is not a Date or a String");
    }
    // Convert to Date if it's a String.
    let date = (typeof dateInput === "string") ? new Date(dateInput) : dateInput;

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

// Just short of a custom HTML tag.
// A class to represent a filesystem object.
class FsObject {
    // url: The URL of the object. Don't include the origin.
    // thumbUrl: The URL of the thumbnail of the object. Don't include the origin.
    // name: The name of the object.
    // lastModified: The last modified date of the object. null if not applicable.
    // (never use undefined for any of them.)
    constructor(url, thumbUrl, name, lastModified, directory) {
        // If any is undefined, throw an error.
        if (typeof url === "undefined") {
            throw new Error("url is undefined");
        }
        if (typeof thumbUrl === "undefined") {
            throw new Error("thumbUrl is undefined");
        }
        if (typeof name === "undefined") {
            throw new Error("name is undefined");
        }
        if (typeof lastModified === "undefined") {
            throw new Error("lastModified is undefined");
        }
        if (typeof directory === "undefined") {
            throw new Error("directory is undefined");
        }

        this.url = url;
        this.thumbUrl = thumbUrl;
        this.name = name;
        this.lastModified = lastModified;
        this.directory = directory
    }

    // Render
    render() {
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

        // Real URLs
        // Prefix
        const prefix = this.directory ? "/user" : listOrigin;
        const realUrl = prefix + this.url;
        const realThumbUrl = thumbOrigin + this.thumbUrl;

        const li = document.createElement("li");
        const file = document.createElement("div");
        file.classList.add("file");
        const thumb = document.createElement("a");
        thumb.classList.add("thumb");
        thumb.href = realUrl;
        const img = document.createElement("img");
        img.src = realThumbUrl;
        img.alt = "";
        img.width = 32;
        img.height = 32;
        thumb.appendChild(img);
        file.appendChild(thumb);
        const info = document.createElement("div");
        info.classList.add("info");
        const filename = document.createElement("a");
        filename.classList.add("filename");
        filename.href = realUrl;
        filename.textContent = this.name;
        info.appendChild(filename);
        if (this.lastModified !== null) {
            const lastModifiedStr = shortFormatDateA_en_US(this.lastModified);
            const byline = document.createElement("div");
            byline.classList.add("byline");
            byline.textContent = lastModifiedStr;
            info.appendChild(byline);
        }
        file.appendChild(info);
        li.appendChild(file);

        // Wow, I really don't want to do this.
        // See, this is why there are such things as frameworks.
        // This is messed up.
        return li;
    }
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

    let item = new FsObject("/", "/thumbdir", "/", null, true);
    let li = item.render();

    return li;
}

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

    let item = new FsObject(parentUrl, "/thumbdir", "..", null, true);
    let li = item.render();

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

    // Inject the back row (..) link.
    // Disable the back (..) link if and only if we are at the root.
    if (extraPath !== "/") {
        const parent = extraPath.substring(0, extraPath.lastIndexOf('/'));
        const li = showParent(parent);
        listFile.appendChild(li);
    }

    // Inject the root link.
    if (extraPath !== "/") {
        const li = showRoot();
        listFile.appendChild(li);
    }

    const loadAndShow = async (extraPath) => {
        // Make a GET request to the server to get the list of files.
        const response = await fetch(`${listOrigin}${extraPath}`);

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
        if (json.version[0] !== '0' || json.version[1] !== '2') {
            throw new Error(`Client supports 0.2.*. Server: (json.version = \"${json.version}\")
incompatible. Please update your client.`);
        }

        const listing = json.listing ? json.listing : [];
        const files = listing.files ? listing.files : [];
        const directories = listing.directories ? listing.directories : [];

        // Sort by last modified date, from newest to oldest
        directories.sort((a, b) => new Date(b.last_modified) - new Date(a.last_modified));
        files.sort((a, b) => new Date(b.last_modified) - new Date(a.last_modified));

        // Add the rows to the table.
        for (const directory of directories) {
            // let li = showEntry(directory);
            let item = new FsObject(directory.url, directory.thumb_url, directory.name, directory.last_modified, true);
            let li = item.render();
            listFile.appendChild(li);
        }
        for (const file of files) {
            let item = new FsObject(file.url, file.thumb_url, file.name, file.last_modified, false);
            let li = item.render();
            listFile.appendChild(li);
        }
    };

    loadAndShow(extraPath);
});
