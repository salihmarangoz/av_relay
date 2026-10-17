// SPDX-License-Identifier: GPL-3.0-or-later
// The cameras and microphones the user can choose from.

import AVFoundation

public struct CaptureDeviceInfo: Sendable, Equatable, Identifiable {
    public var id: String
    public var name: String
    public var isBuiltIn: Bool
}

public enum CaptureDevices {
    public static func cameras() -> [CaptureDeviceInfo] {
        AVCaptureDevice.DiscoverySession(deviceTypes: [.builtInWideAngleCamera, .external, .continuityCamera],
                                         mediaType: .video, position: .unspecified).devices.map(info)
    }

    public static func microphones() -> [CaptureDeviceInfo] {
        AVCaptureDevice.DiscoverySession(deviceTypes: [.microphone], mediaType: .audio, position: .unspecified).devices.map(info)
    }

    /// The device used when the user hasn't chosen one: the built-in one.
    public static func defaultCamera() -> CaptureDeviceInfo? { CameraCapture.captureDevice(id: nil).map(info) }
    public static func defaultMicrophone() -> CaptureDeviceInfo? { MicrophoneDevice.captureDevice(id: nil).map(info) }

    private static func info(_ device: AVCaptureDevice) -> CaptureDeviceInfo {
        CaptureDeviceInfo(id: device.uniqueID, name: device.localizedName,
                          isBuiltIn: device.transportType == Int32(bitPattern: kAudioDeviceTransportTypeBuiltIn))
    }
}
