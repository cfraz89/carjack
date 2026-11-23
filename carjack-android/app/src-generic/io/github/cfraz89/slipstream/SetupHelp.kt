package io.github.cfraz89.slipstream

/** What each setup step on the main screen is for, and how to see to it by hand. */
object SetupHelp {
    val installShizuku = Help(
        "Install Shizuku",
        "Shizuku lets Slipstream do what an ordinary app can't: give the head unit a screen of its own " +
            "size, pass the head unit's touches and buttons to it, and switch the phone's Wi-Fi over to " +
            "the head unit's. It's free and open source.",
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

    /** On the start screen, where it says what Shizuku is still for. */
    val shizukuStopped = Help(
        "Start Shizuku",
        "Slipstream mode needs Shizuku, to give the head unit a screen of its own. Android Auto doesn't, " +
            "once Slipstream has read the phone's Bluetooth address through it. Shizuku stops " +
            "whenever the phone restarts.",
        startShizuku.steps,
    )

    val installAndroidAuto = Help(
        "Install Android Auto",
        "Outside Slipstream mode, the head unit gets Android Auto, which comes from Google's Android Auto app.",
        listOf(
            "Tap Install to open Android Auto in the Play Store, and install it.",
            "Open it once and accept what it asks.",
        ),
    )

    val headUnitServer = Help(
        "Start Android Auto's head unit server",
        "Outside Slipstream mode, Slipstream gets Android Auto through the head unit server it runs on the " +
            "phone, and found it wasn't running when the head unit last connected. Android Auto only " +
            "runs it when asked, and stops it when the phone restarts or Android Auto updates.",
        listOf(
            "Tap Open to open Android Auto's settings.",
            "The first time only: scroll to the bottom, tap Version ten times, and confirm, to turn " +
                "on developer settings.",
            "Tap ⋮ at the top right, then Start head unit server. If it's running already, stop it " +
                "and start it again.",
            "Leave its notification be. The warning goes once the head unit next connects.",
        ),
    )

    val modes = Help(
        "Modes",
        "What the head unit gets when it connects. A change takes effect straight away, mid-session too.",
        listOf(
            "Android Auto: Google's car-centric infotainment UI. Only some apps support it. Needs " +
                "Android Auto's head unit server running.",
            "Slipstream mode: projects all your apps, the same as on your phone, with a custom " +
                "launcher. Needs Shizuku running.",
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
        "Bluetooth and nearby devices to find and talk to the head unit, and to join its Wi-Fi; " +
            "notifications for Slipstream's status; photos for the launcher's wallpaper.",
        listOf(
            "Tap Allow, and allow each one Android asks about.",
            "If one was refused before, allow it in Settings → Apps → Slipstream → Permissions.",
        ),
    )

    val bluetooth = Help(
        "Bluetooth",
        "The head unit finds the phone, and starts each session, over Bluetooth.",
        listOf("Tap Turn on, or turn Bluetooth on from Quick Settings."),
    )

    val tailscale = Help(
        "Tailscale",
        "With Tailscale on, Slipstream's traffic goes through it and can't reach the head unit's Wi-Fi, " +
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
        "With a VPN on, Slipstream's traffic goes through it and can't reach the head unit's Wi-Fi, " +
            "so the picture never starts.",
        listOf(
            "Open your VPN app's settings.",
            "Look for split tunneling, or excluded apps, and exclude Slipstream.",
            "If it has no such setting, turn the VPN off while riding.",
        ),
    )
}
