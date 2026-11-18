# Isolated Python 3 standard-library adapter. Archives are data, never executed.
import ctypes, hashlib, json, os, platform, stat, sys, tempfile, time, zipfile

def require(ok, message):
    if not ok:
        raise ValueError(message)

def identity(path):
    st = os.stat(path, follow_symlinks=False)
    require(stat.S_ISDIR(st.st_mode), 'Parent is not an ordinary directory')
    return [st.st_dev, st.st_ino]

def commit(stage, target):
    # Kernel-exclusive directory rename: no check-then-overwrite fallback.
    if sys.platform == 'win32':
        os.rename(stage, target)  # Windows refuses an existing destination.
        return
    libc = ctypes.CDLL(None, use_errno=True)
    if sys.platform == 'darwin':
        fn = libc.renamex_np
        fn.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
        result = fn(os.fsencode(stage), os.fsencode(target), 4)  # RENAME_EXCL
    elif sys.platform.startswith('linux'):
        fn = libc.renameat2
        fn.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
        result = fn(-100, os.fsencode(stage), -100, os.fsencode(target), 1)  # RENAME_NOREPLACE
    else:
        raise ValueError('No supported exclusive directory commit on this platform')
    if result:
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error))

def safe_mode(mode):
    # Keep rwx bits, but never group/other write, and always owner read/write so files stay editable.
    return ((mode & 0o755) | 0o600) if mode & 0o777 else 0o644

def zip_mtime(date_time):
    try:
        return int(time.mktime(tuple(date_time) + (0, 0, -1)))
    except (OverflowError, ValueError):
        return None

def validate(archive):
    require(os.path.getsize(archive) <= 8 << 20, 'Archive exceeds 8 MiB')
    with open(archive, 'rb') as stream:
        digest = hashlib.sha256(stream.read()).hexdigest()
    manifest, contents, spellings, entries, directories, total = [], {}, {}, {}, [], 0
    with zipfile.ZipFile(archive) as z:
        require(0 < len(z.infolist()) <= 512, 'Archive must contain 1–512 entries')
        for info in z.infolist():
            name = info.filename.rstrip('/')
            parts = name.split('/')
            require(name and len(name) <= 512 and not name.startswith('/') and '\\' not in name, 'Unsafe archive path')
            require(all(part and part not in ('.', '..', '.git', '.idea', '.openidea', '.vscode') and not part.endswith(('.', ' ')) and all(c.isascii() and (c.isalnum() or c in '._-') for c in part) for part in parts), 'Unsupported archive path')
            for part in parts:
                stem = part.split('.')[0].upper()
                require(stem not in ('CON','PRN','AUX','NUL') and not (len(stem)==4 and stem[:3] in ('COM','LPT') and stem[3] in '123456789'), 'Reserved archive path')
            prefix = ''
            for part in parts:
                prefix = prefix + '/' + part if prefix else part
                previous = spellings.setdefault(prefix.lower(), prefix)
                require(previous == prefix, 'Case-colliding archive paths')
            require(name.lower() not in entries, 'Duplicate archive path')
            entries[name.lower()] = info.is_dir()
            require(not info.flag_bits & 1 and info.compress_type in (zipfile.ZIP_STORED, zipfile.ZIP_DEFLATED), 'Encrypted or unsupported archive')
            mode = info.external_attr >> 16
            kind = stat.S_IFMT(mode)
            require(kind in (0, stat.S_IFREG, stat.S_IFDIR), 'Links and special archive entries are refused')
            require(not (info.is_dir() and kind == stat.S_IFREG) and not (not info.is_dir() and kind == stat.S_IFDIR), 'Conflicting archive type')
            require(not mode & 0o7000, 'Special permissions are refused')
            require(info.file_size <= 4 << 20 and info.file_size <= info.compress_size * 1000 + 1024, 'Archive file exceeds bounds')
            total += info.file_size
            require(total <= 32 << 20, 'Expanded archive exceeds 32 MiB')
            if info.is_dir():
                require(info.file_size == 0, 'Directory has content')
                directories.append(name)
                continue
            data = z.read(info)  # validates CRC before any extraction
            require(len(data) == info.file_size, 'Truncated archive entry')
            permissions = safe_mode(mode)
            manifest.append({'path':name, 'bytes':len(data), 'mode':permissions, 'mtime':zip_mtime(info.date_time), 'sha256':hashlib.sha256(data).hexdigest()})
            contents[name] = data
    for name in entries:
        parts = name.split('/')
        require(not any(entries.get('/'.join(parts[:i])) is False for i in range(1,len(parts))), 'File/directory path conflict')
    require(manifest, 'Archive has no files')
    manifest.sort(key=lambda entry: entry['path'])
    return digest, manifest, sorted(directories), contents

def inspect_tree(target):
    require(os.path.isabs(target) and os.path.realpath(target)==target and not os.path.islink(target), 'Generated tree root is not an ordinary canonical path')
    proof={'target':target,'identity':identity(target),'python':platform.python_version(),'platform':sys.platform}
    files, directories, total = [], [], 0
    for parent, dirs, names in os.walk(target,followlinks=False):
        for name in dirs + names:
            path=os.path.join(parent,name)
            require(not os.path.islink(path), 'Generated tree contains a link')
            relative=os.path.relpath(path,target).replace(os.sep,'/')
            require(len(relative)<=512 and not any(part in ('.git','.idea','.openidea','.vscode') for part in relative.split('/')), 'Generated tree has unsupported metadata/path')
            require(not any(ord(c)<32 or ord(c)==127 for c in relative), 'Generated path contains controls')
            st=os.stat(path,follow_symlinks=False)
            require(not st.st_mode & 0o7000, 'Generated tree contains special permission bits')
            if stat.S_ISDIR(st.st_mode):
                directories.append(relative)
            else:
                require(stat.S_ISREG(st.st_mode) and st.st_size<=4<<20, 'Generated tree contains a special or oversized file')
                flags=os.O_RDONLY|getattr(os,'O_NOFOLLOW',0)
                with os.fdopen(os.open(path,flags),'rb') as stream:
                    data=stream.read((4<<20)+1)
                require(len(data)==st.st_size and len(data)<=4<<20, 'Generated file changed during inspection')
                total+=len(data)
                require(total<=32<<20, 'Generated tree exceeds 32 MiB')
                files.append({'path':relative,'bytes':len(data),'mode':st.st_mode&0o777,'sha256':hashlib.sha256(data).hexdigest()})
            require(len(files)+len(directories)<=512, 'Generated tree has more than 512 entries')
    require(files,'Generated tree is empty')
    files.sort(key=lambda entry:entry['path']);directories.sort()
    proof.update({'files':files,'directories':directories})
    proof['sha256']=hashlib.sha256(json.dumps([files,directories],sort_keys=True).encode()).hexdigest()
    return proof

def main():
    require(sys.version_info >= (3,8), 'Python 3.8 or newer is required')
    request = json.loads(sys.stdin.read(1 << 20))
    if request['operation'] == 'remove-archive':
        archive = request['archive']
        require(os.path.isabs(archive) and os.path.basename(archive) == 'generation-archive.zip', 'Refusing to remove another file')
        try:
            os.remove(archive)
        except FileNotFoundError:
            pass
        print(json.dumps({'removed': True}));return
    if not os.path.isdir(request['target'] if request['operation'] in ('inspect-tree','verify-tree') else request['parent']):
        print(json.dumps({'error':'not-local'}));return
    if request['operation'] in ('inspect-tree','verify-tree'):
        proof=inspect_tree(request['target'])
        if request['operation']=='verify-tree':
            require(proof==request['proof'],'Generated tree changed after review')
        print(json.dumps(proof));return
    archive, parent, artifact = request['archive'], request['parent'], request['artifact']
    require(os.path.isabs(parent), 'Choose an absolute destination parent')
    parent = os.path.realpath(parent)
    require(artifact and len(artifact)<=128 and not artifact.startswith('.') and all(c.isascii() and (c.isalnum() or c in '._-') for c in artifact), 'Invalid artifact directory')
    target = os.path.join(parent, artifact)
    require(not os.path.lexists(target), 'Destination exists; no merge or overwrite')
    proof = {'parent':parent,'identity':identity(parent),'target':target,'python':platform.python_version(),'platform':sys.platform}
    if sys.platform not in ('darwin','win32') and not sys.platform.startswith('linux'):
        raise ValueError('Unsupported archive commit platform')
    if sys.platform not in ('darwin','win32'):
        require(hasattr(ctypes.CDLL(None), 'renameat2'), 'Linux libc lacks exclusive renameat2')
    digest, manifest, directories, contents = validate(archive)
    proof.update({'sha256':digest,'files':manifest,'directories':directories})
    if request['operation'] == 'inspect':
        print(json.dumps(proof)); return
    require(request['operation']=='extract' and request['proof']==proof, 'Archive, destination or adapter profile changed since review')
    stage = tempfile.mkdtemp(prefix='.initializr-',dir=parent)
    try:
        for directory in directories:
            os.makedirs(os.path.join(stage,*directory.split('/')),mode=0o755,exist_ok=True)
        for entry in manifest:
            destination = os.path.join(stage, *entry['path'].split('/'))
            os.makedirs(os.path.dirname(destination), mode=0o755, exist_ok=True)
            with open(destination, 'xb') as stream:
                stream.write(contents[entry['path']])
            os.chmod(destination, entry['mode'])
            if entry['mtime'] is not None:
                os.utime(destination, (entry['mtime'], entry['mtime']))
        require(identity(parent)==proof['identity'], 'Destination parent identity changed')
        commit(stage,target)
        stage = None
        print(json.dumps({'target':target,'sha256':digest,'files':len(manifest),'committed':True}))
    except Exception:
        # Never delete partial output or arbitrary paths after a failed operation.
        print(json.dumps({'committed':False,'staging':stage}),file=sys.stderr)
        raise

try:
    main()
except Exception as error:
    print(type(error).__name__ + ': ' + str(error),file=sys.stderr)
    sys.exit(1)
