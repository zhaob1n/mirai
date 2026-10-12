# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Huang Zhaobin
#
# Hide or show one audio endpoint, as the Sound control panel's Disable does, without
# elevation: a desktop with no output device, like a hosted runner.
#
#   Get-PnpDevice -Class AudioEndpoint -Status OK      # InstanceId SWD\MMDEVAPI\<Id>
#   windows-audio-endpoint.ps1 -Id '{0.0.0.00000000}.{GUID}' -Visible 0
#   windows-audio-endpoint.ps1 -Id '{0.0.0.00000000}.{GUID}' -Visible 1
#
# IPolicyConfig is undocumented but stable since Windows 7; mmsys.cpl uses it.
param([Parameter(Mandatory)][string]$Id, [Parameter(Mandatory)][int]$Visible)
Add-Type -TypeDefinition @"
using System.Runtime.InteropServices;
[Guid("f8679f50-850a-41cf-9c72-430f290290c8"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
interface IPolicyConfig {
    int GetMixFormat(); int GetDeviceFormat(); int ResetDeviceFormat(); int SetDeviceFormat();
    int GetProcessingPeriod(); int SetProcessingPeriod(); int GetShareMode(); int SetShareMode();
    int GetPropertyValue(); int SetPropertyValue(); int SetDefaultEndpoint();
    [PreserveSig] int SetEndpointVisibility([MarshalAs(UnmanagedType.LPWStr)] string id, int visible);
}
[ComImport, Guid("870af99c-171d-4f9e-af0d-e63df40c2bc9")] class PolicyConfigClient {}
public static class MiraiEndpoint {
    public static int Show(string id, int visible) {
        return ((IPolicyConfig)new PolicyConfigClient()).SetEndpointVisibility(id, visible);
    }
}
"@
$hr = [MiraiEndpoint]::Show($Id, $Visible)
if ($hr -ne 0) { throw ("SetEndpointVisibility failed: 0x{0:X8}" -f $hr) }
