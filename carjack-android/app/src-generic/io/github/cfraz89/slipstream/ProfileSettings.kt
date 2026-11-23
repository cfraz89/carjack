package io.github.cfraz89.slipstream

import android.content.Context
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import io.github.cfraz89.slipstream.ui.R as UiR

/**
 * The selected profile, whose settings the screen shows, picked from the rest; and the button to
 * add, edit and delete them, [onEdit]. [onChange] has the screen read its settings afresh.
 */
@Composable
fun ProfileBar(context: Context, onEdit: () -> Unit, onChange: () -> Unit) {
    var picking by remember { mutableStateOf(false) }
    val selected = Profiles.selected(context)
    Heading("Profile")
    Row(
        Modifier.fillMaxWidth().padding(start = 16.dp, end = 8.dp, bottom = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Row(
            Modifier
                .weight(1f)
                .heightIn(min = 48.dp)
                .clip(RoundedCornerShape(24.dp))
                .background(MaterialTheme.colorScheme.surfaceContainerHigh)
                .clickable { picking = true }
                .padding(horizontal = 16.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Icon(painterResource(selected.icon.res), null, Modifier.size(20.dp))
            Spacer(Modifier.width(12.dp))
            Text(
                selected.name, Modifier.weight(1f),
                style = MaterialTheme.typography.titleMedium, maxLines = 1, overflow = TextOverflow.Ellipsis,
            )
            Icon(painterResource(UiR.drawable.ic_sl_expand), null)
        }
        IconButton(onClick = onEdit) {
            Icon(painterResource(UiR.drawable.ic_sl_edit), "Profiles")
        }
    }
    if (picking) {
        val all = Profiles.all(context)
        ChoiceDialog("Profile", all.map { it.name }, all.indexOfFirst { it.id == selected.id }, { picking = false }) {
            Profiles.select(context, all[it].id)
            picking = false
            onChange()
        }
    }
}

/** A profile open for its name and icon: [profile] null for a new one. */
private class EditingProfile(val profile: Profile?)

/**
 * Each profile, a tap selecting it; a long press picks some to edit or delete from the app bar;
 * and a new one to add, named first.
 */
@Composable
fun ProfilesScreen(context: Context, onClose: () -> Unit) {
    var revision by remember { mutableIntStateOf(0) }
    var editing by remember { mutableStateOf<EditingProfile?>(null) }
    var picked by remember { mutableStateOf(emptySet<String>()) }
    var confirming by remember { mutableStateOf(false) }
    @Suppress("UNUSED_EXPRESSION") revision
    val all = Profiles.all(context)
    val selected = Profiles.selected(context).id
    val devices = remember { pairedDevices(context) }
    Dialog(onDismissRequest = onClose, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Page {
            if (picked.isEmpty()) {
                TopBar("Profiles", onClose)
            } else {
                BackHandler { picked = emptySet() }
                Row(Modifier.heightIn(min = 64.dp), verticalAlignment = Alignment.CenterVertically) {
                    IconButton(onClick = { picked = emptySet() }) {
                        Icon(painterResource(UiR.drawable.ic_sl_close), "Cancel")
                    }
                    Spacer(Modifier.width(8.dp))
                    Text("${picked.size} selected", Modifier.weight(1f), style = MaterialTheme.typography.titleLarge)
                    if (picked.size == 1) {
                        IconButton(onClick = { editing = EditingProfile(all.first { it.id in picked }) }) {
                            Icon(painterResource(UiR.drawable.ic_sl_edit), "Edit")
                        }
                    }
                    // At least one has to stay.
                    IconButton(onClick = { confirming = true }, enabled = picked.size < all.size) {
                        Icon(painterResource(UiR.drawable.ic_sl_delete), "Delete")
                    }
                }
            }
            SectionCard(MaterialTheme.colorScheme.surfaceContainer) {
                for (profile in all) {
                    fun toggle() {
                        picked = if (profile.id in picked) picked - profile.id else picked + profile.id
                    }
                    ListRow(
                        Leading.Glyph(profile.icon.res),
                        profile.name,
                        deviceName(Profiles.targetSetting(context, profile.id), devices),
                        trailing = {
                            if (profile.id == selected) {
                                Icon(painterResource(UiR.drawable.ic_sl_check), "Selected", tint = MaterialTheme.colorScheme.primary)
                            }
                        },
                        supportingIcon = UiR.drawable.ic_sl_bluetooth,
                        selected = profile.id in picked,
                        onLongClick = { toggle() },
                    ) {
                        if (picked.isNotEmpty()) {
                            toggle()
                        } else {
                            Profiles.select(context, profile.id)
                            revision++
                        }
                    }
                }
            }
            if (picked.isEmpty()) {
                Row(Modifier.padding(start = 16.dp, top = 12.dp)) {
                    FilledButton("Add profile") { editing = EditingProfile(null) }
                }
            }
        }
        editing?.let { e ->
            ProfileDialog(
                e.profile,
                Profiles.defaultName(all.size),
                onDone = { name, icon ->
                    if (e.profile == null) Profiles.add(context, name, icon) else Profiles.update(context, e.profile.id, name, icon)
                    editing = null
                    picked = emptySet()
                    revision++
                },
                onDismiss = { editing = null },
            )
        }
        if (confirming) {
            val names = all.filter { it.id in picked }.map { it.name }
            AlertDialog(
                onDismissRequest = { confirming = false },
                title = { Text(if (names.size == 1) "Delete ${names[0]}?" else "Delete ${names.size} profiles?") },
                confirmButton = {
                    TextButton(onClick = {
                        Profiles.remove(context, picked)
                        picked = emptySet()
                        confirming = false
                        revision++
                    }) { Text("Delete") }
                },
                dismissButton = { TextButton(onClick = { confirming = false }) { Text("Cancel") } },
            )
        }
    }
}

/** A profile's name and icon: [profile]'s to edit, or a new one's, offered [defaultName]. */
@Composable
private fun ProfileDialog(
    profile: Profile?,
    defaultName: String,
    onDone: (String, ProfileIcon) -> Unit,
    onDismiss: () -> Unit,
) {
    var name by remember { mutableStateOf(profile?.name ?: defaultName) }
    var icon by remember { mutableStateOf(profile?.icon ?: ProfileIcon.SCREEN) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(if (profile == null) "New profile" else "Edit profile") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(16.dp)) {
                OutlinedTextField(name, { name = it }, label = { Text("Name") }, singleLine = true)
                Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    for (choice in ProfileIcon.entries) {
                        val chosen = choice == icon
                        Column(
                            Modifier
                                .clip(RoundedCornerShape(16.dp))
                                .background(
                                    if (chosen) MaterialTheme.colorScheme.secondaryContainer else MaterialTheme.colorScheme.surfaceContainerHigh,
                                )
                                .clickable { icon = choice }
                                .padding(horizontal = 16.dp, vertical = 12.dp),
                            horizontalAlignment = Alignment.CenterHorizontally,
                        ) {
                            Icon(painterResource(choice.res), null)
                            Text(choice.title, style = MaterialTheme.typography.labelMedium)
                        }
                    }
                }
            }
        },
        confirmButton = {
            TextButton(onClick = { onDone(name, icon) }) { Text(if (profile == null) "Add" else "Save") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}
