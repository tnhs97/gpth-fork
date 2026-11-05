import 'dart:ffi';
import 'dart:io';
import 'dart:math';

import 'package:collection/collection.dart';
import 'package:ffi/ffi.dart';
import 'package:gpth/interactive.dart' as interactive;
import 'package:mime/mime.dart';
import 'package:path/path.dart' as p;
import 'package:proper_filesize/proper_filesize.dart';

import 'media.dart';

// remember to bump this
const version = '3.5.0';

/// max file size to read for exif/hash/anything
const maxFileSize = 64 * 1024 * 1024;

/// convenient print for errors
void error(Object? object) => stderr.write('$object\n');

Never quit([int code = 1]) {
  if (interactive.indeed) {
    print('[gpth ${code != 0 ? 'quitted :(' : 'finished :)'} (code $code) - '
        'press enter to close]');
    stdin.readLineSync();
  }
  exit(code);
}

/// Checks whether [file] is a photo or video
/// Also handles files without extensions (like Google Takeout motion companion videos)
/// by inspecting their magic bytes.
bool isPhotoOrVideo(File file) {
  final mime = lookupMimeType(file.path);
  if (mime != null) {
    return mime.startsWith('image/') ||
        mime.startsWith('video/') ||
        // https://github.com/TheLastGimbus/GooglePhotosTakeoutHelper/issues/223
        // https://github.com/dart-lang/mime/issues/102
        // 🙃🙃
        mime == 'model/vnd.mts';
  }
  if (file.path.endsWith('.json')) return false;
  // Inspect magic bytes for extensionless media files
  try {
    final length = file.lengthSync();
    if (length < 8) return false;
    final raf = file.openSync(mode: FileMode.read);
    final headerBytes = raf.readSync(min(length, 32));
    raf.closeSync();
    final headerMime = lookupMimeType('', headerBytes: headerBytes) ?? '';
    if (headerMime.startsWith('image/') ||
        headerMime.startsWith('video/') ||
        headerMime == 'model/vnd.mts') {
      return true;
    }
    // Check for ftyp (MP4, MOV, HEIC, etc.)
    if (headerBytes.length >= 8 &&
        headerBytes[4] == 0x66 && // 'f'
        headerBytes[5] == 0x74 && // 't'
        headerBytes[6] == 0x79 && // 'y'
        headerBytes[7] == 0x70) { // 'p'
      return true;
    }
    // Check for JPEG (\xFF\xD8\xFF)
    if (headerBytes.length >= 3 &&
        headerBytes[0] == 0xFF &&
        headerBytes[1] == 0xD8 &&
        headerBytes[2] == 0xFF) {
      return true;
    }
    // Check for PNG (\x89PNG)
    if (headerBytes.length >= 4 &&
        headerBytes[0] == 0x89 &&
        headerBytes[1] == 0x50 &&
        headerBytes[2] == 0x4E &&
        headerBytes[3] == 0x47) {
      return true;
    }
  } catch (_) {
    return false;
  }
  return false;
}

extension X on Iterable<FileSystemEntity> {
  /// Easy extension allowing you to filter for files that are photo or video
  Iterable<File> wherePhotoVideo() => whereType<File>().where(isPhotoOrVideo);
}

extension Y on Stream<FileSystemEntity> {
  /// Easy extension allowing you to filter for files that are photo or video
  Stream<File> wherePhotoVideo() => whereType<File>().where(isPhotoOrVideo);
}

extension Util on Stream {
  Stream<T> whereType<T>() => where((e) => e is T).cast<T>();
}

Future<int?> getDiskFree([String? path]) async {
  path ??= Directory.current.path;
  if (Platform.isLinux) {
    return _dfLinux(path);
  } else if (Platform.isWindows) {
    return _dfWindoza(path);
  } else if (Platform.isMacOS) {
    return _dfMcOS(path);
  } else {
    return null;
  }
}

Future<int?> _dfLinux(String path) async {
  final res = await Process.run('df', ['-B1', '--output=avail', path]);
  return res.exitCode != 0
      ? null
      : int.tryParse(
          res.stdout.toString().split('\n').elementAtOrNull(1) ?? '',
          radix: 10, // to be sure
        );
}

Future<int?> _dfWindoza(String path) async {
  try {
    final kernel32 = DynamicLibrary.open('kernel32.dll');
    final getDiskFree = kernel32.lookupFunction<
        Int32 Function(Pointer<Utf16>, Pointer<Uint64>, Pointer<Uint64>,
            Pointer<Uint64>),
        int Function(Pointer<Utf16>, Pointer<Uint64>, Pointer<Uint64>,
            Pointer<Uint64>)>('GetDiskFreeSpaceExW');
    final root = p.rootPrefix(p.absolute(path));
    final pathPtr = root.toNativeUtf16();
    final freeBytes = calloc<Uint64>();
    final totalBytes = calloc<Uint64>();
    final totalFreeBytes = calloc<Uint64>();
    final result =
        getDiskFree(pathPtr, freeBytes, totalBytes, totalFreeBytes);
    final free = result != 0 ? freeBytes.value : null;
    calloc.free(pathPtr);
    calloc.free(freeBytes);
    calloc.free(totalBytes);
    calloc.free(totalFreeBytes);
    if (free != null) return free;
  } catch (_) {}

  // Fallback to PowerShell (for environments where wmic was removed, e.g. Win 11 24H2)
  try {
    final drive = p.rootPrefix(p.absolute(path)).replaceAll('\\', '');
    final res = await Process.run('powershell.exe', [
      '-NoProfile',
      '-NonInteractive',
      '-Command',
      '[System.IO.DriveInfo]::new("$drive").AvailableFreeSpace',
    ]);
    if (res.exitCode == 0) {
      return int.tryParse(res.stdout.toString().trim());
    }
  } catch (_) {}
  return null;
}

Future<int?> _dfMcOS(String path) async {
  final res = await Process.run('df', ['-k', path]);
  if (res.exitCode != 0) return null;
  final line2 = res.stdout.toString().split('\n').elementAtOrNull(1);
  if (line2 == null) return null;
  final elements = line2.split(' ')..removeWhere((e) => e.isEmpty);
  final macSays = int.tryParse(
    elements.elementAtOrNull(3) ?? '',
    radix: 10, // to be sure
  );
  return macSays != null ? macSays * 1024 : null;
}

String filesize(int bytes) => ProperFilesize.generateHumanReadableFilesize(
      bytes,
      base: Bases.Binary,
      decimals: 2,
    );

int outputFileCount(List<Media> media, String albumOption) {
  if (['shortcut', 'duplicate-copy'].contains(albumOption)) {
    return media.fold(0, (prev, e) => prev + e.files.length);
  } else if (albumOption == 'json') {
    return media.length;
  } else if (albumOption == 'nothing') {
    return media.where((e) => e.files.containsKey(null)).length;
  } else {
    throw ArgumentError.value(albumOption, 'albumOption');
  }
}

extension Z on String {
  /// Returns same string if pattern not found
  String replaceLast(String from, String to) {
    final lastIndex = lastIndexOf(from);
    if (lastIndex == -1) return this;
    return replaceRange(lastIndex, lastIndex + from.length, to);
  }
}
