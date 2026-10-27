package io.github.cfraz89.slipstream

/** What each setup step on the main screen is for, and how to see to it by hand. */
object SetupHelp {
    val installShizuku = Help(
        "Install Shizuku",
        "Shizuku lets Slipstream do what an ordinary app can't: give the unit a screen of its own " +
            "size, pass the unit's touches and buttons to it, and switch the phone's Wi-Fi over to " +
            "the unit's. It's free and open source.",
        listOf(
            "Tap Install to open Shizuku in the Play Store, and install it.",
            "Open Shizuku and follow \"Start via Wireless debugging\". It walks you through " +
                "pairing it once, through a notification.",
            "Come back to Slipstream: the step goes once Shizuku is running.",
        ),
    )

    val startShizuku = Help(
        "Start Shizuku",
        "Shizuku stops whenever the phone restarts, and needs starting again before Slipstream " +
            "can use it.",
        listOf(
            "Connect the phone to Wi-Fi: Wireless debugging needs it, just to start.",
            "Tap Open, and in Shizuku tap Start under \"Start via Wireless debugging\".",
            "If it asks, turn on Wireless debugging in Developer options and tap Start again.",
        ),
    )

    val allowShizuku = Help(
        "Let Slipstream use Shizuku",
        "Shizuku asks before an app may use it.",
        listOf(
            "Tap Allow.",
            "In Shizuku's prompt, choose \"Allow all the time\".",
        ),
    )

    val permissions = Help(
        "Permissions",
        "Bluetooth and nearby devices to find and talk to the unit, and to join its Wi-Fi; " +
            "notifications for Slipstream's status; photos for the launcher's wallpaper.",
        listOf(
            "Tap Allow, and allow each one Android asks about.",
            "If one was refused before, allow it in Settings → Apps → Slipstream → Permissions.",
        ),
    )

    val bluetooth = Help(
        "Bluetooth",
        "The unit finds the phone, and starts each session, over Bluetooth.",
        listOf("Tap Turn on, or turn Bluetooth on from Quick Settings."),
    )

    val trustedDevice = Help(
        "Trusted device",
        "While the phone is locked, Android hides every app on the unit but Slipstream's own " +
            "screens. With the unit as a trusted device, the phone needs no PIN while it's " +
            "connected, so Slipstream can clear the lock screen by itself.",
        listOf(
            "Connect to the unit, so it's in range and on Bluetooth.",
            "Tap Settings, or open Settings and search for \"Trusted devices\".",
            "Under Extend Unlock → Trusted devices, add a trusted device and pick the unit.",
            "Unlock the phone once after adding it: trust starts from an unlock.",
        ),
    )

    val powerButton = Help(
        "Power button lock",
        "Slipstream holds off the phone's own locking while the unit is connected, but a power " +
            "button set to lock instantly still locks it, which hides the unit's apps.",
        listOf(
            "Tap Settings, then Device unlock.",
            "Tap the gear beside Screen lock.",
            "Turn off \"Power button instantly locks\".",
        ),
    )

    val tailscale = Help(
        "Tailscale",
        "With Tailscale on, Slipstream's traffic goes through it and can't reach the unit's Wi-Fi, " +
            "so the picture never starts. Excluding Slipstream keeps Tailscale for everything else.",
        listOf(
            "Tap Tailscale, then open its settings.",
            "Open \"App-based split tunneling\".",
            "Exclude Slipstream.",
            "Turn Tailscale off and on again if this step stays.",
        ),
    )

    val vpn = Help(
        "VPN",
        "With a VPN on, Slipstream's traffic goes through it and can't reach the unit's Wi-Fi, " +
            "so the picture never starts.",
        listOf(
            "Open your VPN app's settings.",
            "Look for split tunneling, or excluded apps, and exclude Slipstream.",
            "If it has no such setting, turn the VPN off while riding.",
        ),
    )
}
