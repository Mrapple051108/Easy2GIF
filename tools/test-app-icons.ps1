#Requires -Version 5.1
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string[]]$Executable,
    [Parameter(Mandatory = $true)][string]$Icon
)
$ErrorActionPreference = 'Stop'
if (-not ('Easy2GIF.IconResources' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Runtime.InteropServices;

namespace Easy2GIF {
    public static class IconResources {
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        static extern IntPtr LoadLibraryEx(string path, IntPtr file, uint flags);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode)]
        static extern IntPtr FindResource(IntPtr module, IntPtr name, IntPtr type);
        [DllImport("kernel32.dll")]
        static extern uint SizeofResource(IntPtr module, IntPtr resource);
        [DllImport("kernel32.dll")]
        static extern IntPtr LoadResource(IntPtr module, IntPtr resource);
        [DllImport("kernel32.dll")]
        static extern IntPtr LockResource(IntPtr resource);
        [DllImport("kernel32.dll")]
        static extern bool FreeLibrary(IntPtr module);

        static byte[] Read(IntPtr module, int name, int type) {
            var resource = FindResource(module, (IntPtr)name, (IntPtr)type);
            if (resource == IntPtr.Zero) throw new InvalidDataException("Missing icon resource " + name);
            var bytes = new byte[checked((int)SizeofResource(module, resource))];
            var data = LockResource(LoadResource(module, resource));
            if (data == IntPtr.Zero) throw new InvalidDataException("Cannot read icon resource");
            Marshal.Copy(data, bytes, 0, bytes.Length);
            return bytes;
        }

        public static void Verify(string executable, string icon) {
            var source = File.ReadAllBytes(icon);
            var module = LoadLibraryEx(executable, IntPtr.Zero, 2);
            if (module == IntPtr.Zero) throw new IOException("Cannot read EXE resources: " + executable);
            try {
                var group = Read(module, 1, 14);
                int count = BitConverter.ToUInt16(source, 4);
                if (BitConverter.ToUInt16(group, 4) != count)
                    throw new InvalidDataException("EXE icon size count differs: " + executable);
                for (int i = 0; i < count; ++i) {
                    int entry = 6 + i * 16;
                    int length = checked((int)BitConverter.ToUInt32(source, entry + 8));
                    int offset = checked((int)BitConverter.ToUInt32(source, entry + 12));
                    var embedded = Read(module, BitConverter.ToUInt16(group, 6 + i * 14 + 12), 3);
                    if (embedded.Length != length)
                        throw new InvalidDataException("Stale EXE icon: " + executable);
                    for (int j = 0; j < length; ++j)
                        if (embedded[j] != source[offset + j])
                            throw new InvalidDataException("Stale EXE icon: " + executable);
                }
            } finally { FreeLibrary(module); }
        }
    }
}
'@
}
foreach ($path in $Executable) {
    [Easy2GIF.IconResources]::Verify((Resolve-Path -LiteralPath $path).Path, (Resolve-Path -LiteralPath $Icon).Path)
    Write-Host "Verified embedded icon: $path"
}
