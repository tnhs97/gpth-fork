/// This file contains logic/utils for final act of moving actual files once
/// we have everything grouped, de-duplicated and sorted

import 'dart:convert';
import 'dart:ffi';
import 'dart:io';
import 'dart:math';

import 'package:collection/collection.dart';
import 'package:ffi/ffi.dart';
import 'package:gpth/interactive.dart' as interactive;
import 'package:gpth/utils.dart';
import 'package:mime/mime.dart';
import 'package:path/path.dart' as p;

import 'media.dart';

final class FILETIME extends Struct {
  @Uint32()
  external int dwLowDateTime;
  @Uint32()
  external int dwHighDateTime;
}

void _setWindowsFileTimes(File file, DateTime time) {
  try {
    final kernel32 = DynamicLibrary.open('kernel32.dll');
    final createFile = kernel32.lookupFunction<
        IntPtr Function(Pointer<Utf16>, Uint32, Uint32, Pointer<Void>, Uint32,
            Uint32, IntPtr),
        int Function(Pointer<Utf16>, int, int, Pointer<Void>, int, int, int)>(
      'CreateFileW',
    );
    final setFileTime = kernel32.lookupFunction<
        Int32 Function(IntPtr, Pointer<FILETIME>, Pointer<FILETIME>,
            Pointer<FILETIME>),
        int Function(int, Pointer<FILETIME>, Pointer<FILETIME>,
            Pointer<FILETIME>)>(
      'SetFileTime',
    );
    final closeHandle = kernel32.lookupFunction<
        Int32 Function(IntPtr), int Function(int)>(
      'CloseHandle',
    );

    final pathPtr = file.absolute.path.toNativeUtf16();
    // FILE_WRITE_ATTRIBUTES = 0x0100, FILE_SHARE_READ | FILE_SHARE_WRITE = 3, OPEN_EXISTING = 3
    final hFile = createFile(pathPtr, 0x0100, 3, nullptr, 3, 0x80, 0);
    calloc.free(pathPtr);
    if (hFile != -1 && hFile != 0) {
      final utcTime = time.toUtc();
      final fileTimeInt =
          (utcTime.millisecondsSinceEpoch + 11644473600000) * 10000;
      final ft = calloc<FILETIME>();
      ft.ref.dwLowDateTime = fileTimeInt & 0xFFFFFFFF;
      ft.ref.dwHighDateTime = (fileTimeInt >> 32) & 0xFFFFFFFF;
      setFileTime(hFile, ft, nullptr, ft);
      closeHandle(hFile);
      calloc.free(ft);
    }
  } catch (_) {}
}

bool isVideoFile(File file) {
  final mime = lookupMimeType(file.path) ?? '';
  if (mime.startsWith('video/')) return true;
  final ext = p.extension(file.path).toLowerCase();
  const videoExts = {
    '.mp4',
    '.mov',
    '.3gp',
    '.avi',
    '.m4v',
    '.mkv',
    '.webm',
    '.wmv'
  };
  if (videoExts.contains(ext)) return true;
  if (ext.isEmpty) {
    try {
      final length = file.lengthSync();
      if (length >= 8) {
        final raf = file.openSync(mode: FileMode.read);
        final headerBytes = raf.readSync(min(length, 32));
        raf.closeSync();
        if (headerBytes.length >= 8 &&
            headerBytes[4] == 0x66 &&
            headerBytes[5] == 0x74 &&
            headerBytes[6] == 0x79 &&
            headerBytes[7] == 0x70) {
          return true;
        }
      }
    } catch (_) {}
  }
  return false;
}

String resolveDestinationFilename(File file) {
  var name = p.basename(file.path);
  if (p.extension(name).isEmpty) {
    try {
      final length = file.lengthSync();
      if (length >= 8) {
        final raf = file.openSync(mode: FileMode.read);
        final headerBytes = raf.readSync(min(length, 32));
        raf.closeSync();
        if (headerBytes.length >= 8 &&
            headerBytes[4] == 0x66 &&
            headerBytes[5] == 0x74 &&
            headerBytes[6] == 0x79 &&
            headerBytes[7] == 0x70) {
          return '$name.mp4';
        }
        if (headerBytes.length >= 3 &&
            headerBytes[0] == 0xFF &&
            headerBytes[1] == 0xD8 &&
            headerBytes[2] == 0xFF) {
          return '$name.jpg';
        }
        if (headerBytes.length >= 4 &&
            headerBytes[0] == 0x89 &&
            headerBytes[1] == 0x50 &&
            headerBytes[2] == 0x4E &&
            headerBytes[3] == 0x47) {
          return '$name.png';
        }
      }
    } catch (_) {}
  }
  return name;
}

/// This will add (1) add end of file name over and over until file with such
/// name doesn't exist yet. Will leave without "(1)" if is free already
File findNotExistingName(File initialFile) {
  var file = initialFile;
  while (file.existsSync()) {
    file = File('${p.withoutExtension(file.path)}(1)${p.extension(file.path)}');
  }
  return file;
}

/// This will create symlink on unix and shortcut on windoza
///
/// Uses [findNotExistingName] for safety
Future<File> createShortcut(Directory location, File target) async {
  final name = '${p.basename(target.path)}${Platform.isWindows ? '.lnk' : ''}';
  final link = findNotExistingName(File(p.join(location.path, name)));
  // this must be relative to not break when user moves whole folder around:
  // https://github.com/TheLastGimbus/GooglePhotosTakeoutHelper/issues/232
  final targetRelativePath = p.relative(target.path, from: link.parent.path);
  if (Platform.isWindows) {
    final escapedLink = link.path.replaceAll("'", "''");
    final escapedTarget = targetRelativePath.replaceAll("'", "''");
    final psCommand =
        "\$ws = New-Object -ComObject WScript.Shell; "
        "\$s = \$ws.CreateShortcut('$escapedLink'); "
        "\$s.TargetPath = '$escapedTarget'; "
        "\$s.WorkingDirectory = [System.IO.Path]::GetDirectoryName('$escapedTarget'); "
        "\$s.Save()";
    // Convert to UTF-16LE and Base64 for -EncodedCommand to avoid non-ASCII codepage issues
    final bytes = <int>[];
    for (final unit in psCommand.codeUnits) {
      bytes.add(unit & 0xFF);
      bytes.add((unit >> 8) & 0xFF);
    }
    final encoded = base64.encode(bytes);
    final res = await Process.run(
      'powershell.exe',
      [
        '-ExecutionPolicy',
        'Bypass',
        '-NoLogo',
        '-NonInteractive',
        '-NoProfile',
        '-EncodedCommand',
        encoded,
      ],
    );
    if (res.exitCode != 0) {
      throw 'PowerShell shortcut creation failed (exit ${res.exitCode}): ${res.stderr}';
    }
    return File(link.path);
  } else {
    return File((await Link(link.path).create(targetRelativePath)).path);
  }
}

/// Big-ass logic of moving files from input to output
///
/// [allMediaFinal] should be nice, de-duplicated and album-ed etc
///
/// [copy] indicates whether to copy files or move them
///
/// [divideToDates] will divide to dates even inside albums!
///
/// [albumBehavior] must be one of [interactive.albumOptions]
///
/// [flat] outputs all files directly without year/month subfolders
///
/// [splitMedia] separates photos into ALL_PHOTOS and videos into ALL_VIDEOS
///
/// Emits number of files that it copied/created/whatever (starting from 1) -
/// use [outputFileCount] function for progress measurement
Stream<int> moveFiles(
  List<Media> allMediaFinal,
  Directory output, {
  required bool copy,
  required bool divideToDates,
  required String albumBehavior,
  bool flat = false,
  bool splitMedia = false,
}) async* {
  assert(interactive.albumOptions.keys.contains(albumBehavior));

  /// used only in 'json' behavior
  /// key = name of main outputted file | value = list of albums it belongs to
  final infoJson = <String, List<String>>{};
  var i = 0;
  for (final m in allMediaFinal) {
    // main file shortcuts will link to
    File? mainFile;

    final nullFirst = albumBehavior == 'json'
        // in 'json' case, we want to copy ALL files (like Archive) as normals
        ? [MapEntry(null, m.files.values.first)]
        // this will put null media first so album shortcuts can link to it
        : m.files.entries
            .sorted((a, b) => (a.key ?? '').compareTo(b.key ?? ''));
    // iterate over all media of file to do something about them
    // ignore non-nulls with 'ignore', copy with 'duplicate-copy',
    // symlink with 'shortcut' etc
    for (final file in nullFirst) {
      // if it's not from year folder and we're doing nothing/json, skip
      if (file.key != null && ['nothing', 'json'].contains(albumBehavior)) {
        continue;
      }
      // now on, logic is shared for nothing+null/shortcut/copy cases
      final date = m.dateTaken;

      String mainFolderName;
      if (file.key == null) {
        if (splitMedia) {
          mainFolderName = isVideoFile(file.value) ? 'ALL_VIDEOS' : 'ALL_PHOTOS';
        } else {
          mainFolderName = 'ALL_PHOTOS';
        }
      } else {
        mainFolderName = file.key!.trim();
      }

      final folder = Directory(
        p.join(
          output.path,
          mainFolderName,
          flat
              ? ''
              : date == null
                  ? 'date-unknown'
                  : divideToDates
                      ? p.join(
                          '${date.year}',
                          date.month.toString().padLeft(2, '0'),
                        )
                      : '',
        ),
      );
      // now folder logic is so complex i'll just create it every time 🤷
      await folder.create(recursive: true);

      /// result file/symlink to later change modify time
      File? result;

      /// moves/copies file with safe name
      // it's here because we do this for two cases
      moveFile() async {
        final targetName = resolveDestinationFilename(file.value);
        final freeFile = findNotExistingName(File(p.join(folder.path, targetName)));
        try {
          return copy
              ? await file.value.copy(freeFile.path)
              : await file.value.rename(freeFile.path);
        } on FileSystemException {
          print(
            "Uh-uh, it looks like you selected other output drive than\n"
            "input one - gpth can't move files between them. But, you don't have\n"
            "to do this! Gpth *moves* files, so this doesn't take any extra space!\n"
            "Please run again and select different output location <3",
          );
          quit(1);
        }
      }

      if (file.key == null) {
        // if it's just normal "Photos from .." (null) file, just move it
        result = await moveFile();
        mainFile = result;
      } else if (albumBehavior == 'shortcut' && mainFile != null) {
        try {
          result = await createShortcut(folder, mainFile);
        } catch (e) {
          // in case powershell fails/whatever
          print('Creating shortcut for '
              '${p.basename(mainFile.path)} in ${p.basename(folder.path)} '
              'failed :(\n$e\n - copying normal file instead');
          result = await moveFile();
        }
      } else {
        // else - if we either run duplicate-copy or main file is missing:
        // (this happens with archive/trash/weird situation)
        // just copy it
        result = await moveFile();
      }

      // Done! Now, set the date:

      var time = m.dateTaken ?? DateTime.now();
      if (Platform.isWindows && time.isBefore(DateTime(1970))) {
        print(
            'WARNING: ${m.firstFile.path} has date $time, which is before 1970 '
            '(not supported on Windows) - will be set to 1970-01-01');
        time = DateTime(1970);
      }
      try {
        await result.setLastModified(time);
        if (Platform.isWindows) {
          _setWindowsFileTimes(result, time);
        }
      } on OSError catch (e) {
        // Sometimes windoza throws error but successes anyway 🙃:
        // https://github.com/TheLastGimbus/GooglePhotosTakeoutHelper/issues/229#issuecomment-1685085899
        // That's why this is here
        if (e.errorCode != 0) {
          print("WARNING: Can't set modification time on $result: $e");
        }
      } catch (e) {
        print("WARNING: Can't set modification time on $result: $e");
      }

      // one copy/move/whatever - one yield
      yield ++i;

      if (albumBehavior == 'json') {
        infoJson[p.basename(result.path)] =
            m.files.keys.whereNotNull().toList();
      }
    }
    // done with this media - next!
  }
  if (albumBehavior == 'json') {
    await File(p.join(output.path, 'albums-info.json'))
        .writeAsString(jsonEncode(infoJson));
  }
}
