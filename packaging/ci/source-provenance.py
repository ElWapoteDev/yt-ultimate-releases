"""Bind installers to a clean source commit; also verify a separate local workspace."""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import tarfile
import tomllib


def git(repository, *arguments):
    return subprocess.check_output(['git', '-C', str(repository), *arguments], text=True).strip()


def digest_file(path):
    with path.open('rb') as source:
        return hashlib.file_digest(source, 'sha256').hexdigest()


def compare_archive(source_root, archive):
    count = 0
    for item in archive:
        relative = PurePosixPath(item.name)
        if relative.is_absolute() or '..' in relative.parts:
            raise RuntimeError(f'Unsafe source archive path: {item.name}')
        path = source_root.joinpath(*relative.parts)
        if item.isdir():
            continue
        if item.issym():
            if not path.is_symlink() or os.readlink(path) != item.linkname:
                raise RuntimeError(f'Source symlink differs: {item.name}')
        elif item.isfile():
            if path.is_symlink() or not path.is_file():
                raise RuntimeError(f'Source file is absent or not regular: {item.name}')
            with archive.extractfile(item) as expected:
                expected_hash = hashlib.file_digest(expected, 'sha256').hexdigest()
            if digest_file(path) != expected_hash:
                raise RuntimeError(f'Source file differs from the frozen commit: {item.name}')
        else:
            raise RuntimeError(f'Unsupported source archive entry: {item.name}')
        count += 1
    return count


def verify_source(repository, source_root, expected):
    if not re.fullmatch(r'[0-9a-f]{40}', expected):
        raise RuntimeError('An exact 40-character source commit is required')
    commit = git(repository, 'rev-parse', 'HEAD')
    if commit != expected:
        raise RuntimeError(f'Checked out {commit}, expected {expected}')
    if git(repository, 'status', '--porcelain', '--untracked-files=no'):
        raise RuntimeError('Tracked source files were modified after checkout')
    if repository.resolve() == source_root.resolve():
        # Git verifies clean tracked content with its configured line-ending
        # filters, including native Windows checkouts.
        count = len(git(repository, 'ls-files', '-z').split('\0')) - 1
    else:
        # The local working project has no .git directory. Compare every frozen
        # source entry without extracting archives or touching user data.
        process = subprocess.Popen(['git', '-C', str(repository), 'archive', '--format=tar', 'HEAD'], stdout=subprocess.PIPE)
        try:
            with tarfile.open(fileobj=process.stdout, mode='r|') as archive:
                count = compare_archive(source_root, archive)
            if process.wait() != 0:
                raise RuntimeError('Could not read the frozen Git source archive')
        finally:
            process.stdout.close()
            if process.poll() is None:
                process.terminate()
                process.wait()
    return {'commit': commit, 'tree': git(repository, 'rev-parse', 'HEAD^{tree}'), 'verified_source_files': count}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--git-root', type=Path, default=Path('.'))
    parser.add_argument('--source-root', type=Path, default=Path('.'))
    parser.add_argument('--expected-commit', required=True)
    parser.add_argument('--package-root', type=Path)
    args = parser.parse_args()
    evidence = verify_source(args.git_root, args.source_root, args.expected_commit)
    with (args.source_root / 'Cargo.toml').open('rb') as source:
        version = tomllib.load(source)['workspace']['package']['version']
    evidence.update(schema=1, repository='ElWapoteDev/yt-ultimate-releases', version=version, edition='full')
    if args.package_root:
        directory = args.package_root / version / 'full'
        artifacts = sorted([*directory.glob('*_full.dmg'), *directory.glob('*_full-setup.exe')])
        if len(artifacts) != 1 or version not in artifacts[0].name:
            raise RuntimeError('Expected exactly one installer matching the current Full version')
        model = Path(os.environ['YTU_PACKAGE_STEMS_MODEL'])
        model_hash = digest_file(model)
        if model_hash != '31d794ab054bd70964b6165ceca3b94e00f344f8a0b8bc325def47ce9aa14926':
            raise RuntimeError('The package model differs from the approved model')
        evidence.update(target=args.package_root.name, model={
            'name': 'bs-roformer-v1.pt', 'bytes': model.stat().st_size, 'sha256': model_hash
        }, artifacts=[
            {'name': path.name, 'bytes': path.stat().st_size, 'sha256': digest_file(path)} for path in artifacts
        ])
        output = directory / 'BUILD-SOURCE.json'
        temporary = output.with_suffix('.json.tmp')
        temporary.write_text(json.dumps(evidence, indent=2) + '\n')
        temporary.replace(output)
    print(json.dumps(evidence, indent=2))


if __name__ == '__main__':
    main()
