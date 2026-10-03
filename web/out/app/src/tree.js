/** tree.ts — build a folder tree from repo-relative file paths. */
function compareNames(a, b) {
    if (a.isDir !== b.isDir)
        return a.isDir ? -1 : 1;
    const al = a.name.toLowerCase(), bl = b.name.toLowerCase();
    return al < bl ? -1 : al > bl ? 1 : a.name < b.name ? -1 : a.name > b.name ? 1 : 0;
}
export function buildTree(files, rootName) {
    const root = { name: rootName, path: '', isDir: true, depth: 0, children: [], fileCount: 0, folderCount: 0 };
    const dirIndex = new Map([['', root]]);
    for (const raw of files) {
        const file = raw.replace(/\\/g, '/').replace(/^\/+/, '');
        if (!file)
            continue;
        const parts = file.split('/');
        let parent = root;
        let prefix = '';
        for (let i = 0; i < parts.length - 1; i++) {
            prefix = prefix ? `${prefix}/${parts[i]}` : parts[i];
            let dir = dirIndex.get(prefix);
            if (!dir) {
                dir = { name: parts[i], path: prefix, isDir: true, depth: i + 1, children: [], fileCount: 0, folderCount: 0 };
                dirIndex.set(prefix, dir);
                parent.children.push(dir);
            }
            parent = dir;
        }
        parent.children.push({ name: parts[parts.length - 1], path: file, isDir: false, depth: parts.length, children: [], fileCount: 1, folderCount: 0 });
    }
    (function finish(node) {
        if (!node.isDir)
            return;
        node.children.sort(compareNames);
        let files = 0, folders = 0;
        for (const c of node.children) {
            finish(c);
            files += c.fileCount;
            if (c.isDir)
                folders += 1 + c.folderCount;
        }
        node.fileCount = files;
        node.folderCount = folders;
    })(root);
    return root;
}
