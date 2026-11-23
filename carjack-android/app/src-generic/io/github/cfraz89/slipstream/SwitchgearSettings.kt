package io.github.cfraz89.slipstream

import android.content.Context
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextField
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import io.github.cfraz89.slipstream.ui.R as UiR

/** A mapping open in the editor; [new] until it's first saved. */
private class Editing(val mapping: Mapping, val new: Boolean)

/** The mapping the switchgear follows, for the unit last connected; tapped, the screen to change it. */
@Composable
fun SwitchgearCard(context: Context) {
    var revision by remember { mutableIntStateOf(0) }
    var open by remember { mutableStateOf(false) }
    // Read so a change made on the screen recomposes the row.
    @Suppress("UNUSED_EXPRESSION") revision
    val automatic = Mappings.chosen(context) == null
    SectionCard(MaterialTheme.colorScheme.surfaceContainer) {
        ListRow(
            Leading.Glyph(R.drawable.ic_sl_dpad),
            if (automatic) "Automatic" else Mappings.active(context).name,
            Mappings.unit(context)?.bluetoothName ?: "No head unit yet",
            trailing = { Chevron() },
        ) { open = true }
    }
    if (open) {
        SwitchgearScreen(context) {
            open = false
            revision++
        }
    }
}

/** The unit's own mappings first, then every mapping, searchable. */
@Composable
private fun SwitchgearScreen(context: Context, onClose: () -> Unit) {
    var revision by remember { mutableIntStateOf(0) }
    var query by remember { mutableStateOf("") }
    var editing by remember { mutableStateOf<Editing?>(null) }
    @Suppress("UNUSED_EXPRESSION") revision
    val chosen = Mappings.chosen(context)
    val unit = Mappings.unit(context)
    fun choose(id: String?) {
        Mappings.choose(context, id)
        revision++
    }

    Dialog(onDismissRequest = onClose, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Page {
            TopBar("Switchgear", onClose)
            val ownMappings = Mappings.forUnit(context)
            if (ownMappings.isNotEmpty()) {
                Heading("For ${unit?.bluetoothName ?: "this head unit"}")
                SectionCard(MaterialTheme.colorScheme.surfaceContainer) {
                    for (mapping in ownMappings) {
                        MappingRow(mapping, chosen == mapping.id) { choose(mapping.id) }
                    }
                }
            }

            Heading("All mappings")
            TextField(
                query, { query = it },
                placeholder = { Text("Search mappings") },
                leadingIcon = { Icon(painterResource(UiR.drawable.ic_sl_search), null) },
                singleLine = true,
                shape = RoundedCornerShape(28.dp),
                colors = TextFieldDefaults.colors(
                    focusedContainerColor = MaterialTheme.colorScheme.surfaceContainerHigh,
                    unfocusedContainerColor = MaterialTheme.colorScheme.surfaceContainerHigh,
                    focusedIndicatorColor = Color.Transparent,
                    unfocusedIndicatorColor = Color.Transparent,
                ),
                modifier = Modifier.fillMaxWidth().padding(bottom = 12.dp),
            )
            val automatic = Mappings.automatic(context)
            val auto = Mapping("", "Automatic", automatic.actions, true)
            val matches = (listOf(auto) + Mappings.all(context)).filter {
                query.isBlank() || it.name.contains(query.trim(), ignoreCase = true) ||
                    it.description.contains(query.trim(), ignoreCase = true)
            }
            SectionCard(MaterialTheme.colorScheme.surfaceContainer) {
                if (matches.isEmpty()) ListRow(Leading.Glyph(UiR.drawable.ic_sl_info), "No mappings match")
                for (mapping in matches) {
                    if (mapping === auto) {
                        MappingRow(auto, chosen == null, glyph = UiR.drawable.ic_sl_unit) { choose(null) }
                        continue
                    }
                    MappingRow(
                        mapping, chosen == mapping.id,
                        onEdit = if (mapping.builtIn) null else ({ editing = Editing(mapping, new = false) }),
                    ) { choose(mapping.id) }
                }
            }
            Row(Modifier.padding(start = 16.dp, top = 12.dp)) {
                FilledButton("New custom mapping") {
                    editing = Editing(Mappings.copyOf(Mappings.active(context)), new = true)
                }
            }
        }

        editing?.let { e ->
            MappingEditor(
                e,
                onSave = {
                    Mappings.save(context, it)
                    if (e.new) Mappings.choose(context, it.id)
                    editing = null
                    revision++
                },
                onDelete = {
                    Mappings.delete(context, e.mapping.id)
                    editing = null
                    revision++
                },
                onClose = { editing = null },
            )
        }
    }
}

@Composable
private fun MappingRow(
    mapping: Mapping,
    selected: Boolean,
    glyph: Int? = null,
    onEdit: (() -> Unit)? = null,
    onClick: () -> Unit,
) {
    ListRow(
        Leading.Glyph(glyph ?: if (Action.NEXT in mapping.actions.values) R.drawable.ic_sl_dial else R.drawable.ic_sl_dpad),
        mapping.name,
        mapping.description.ifEmpty { null },
        trailing = {
            Row {
                if (onEdit != null) PlainButton("Edit", onEdit)
                if (selected) Icon(painterResource(UiR.drawable.ic_sl_check), "Chosen", tint = MaterialTheme.colorScheme.primary)
            }
        },
        onClick = onClick,
    )
}

/** Which of a mapping's presses an action is picked for. */
private enum class Press(val title: String) { SINGLE("Inputs"), DOUBLE("Double press"), LONG("Long press") }

/** A custom mapping's name and each input's action, on a press, a double press and a long one, kept on Save. */
@Composable
private fun MappingEditor(editing: Editing, onSave: (Mapping) -> Unit, onDelete: () -> Unit, onClose: () -> Unit) {
    val mapping = editing.mapping
    var name by remember { mutableStateOf(mapping.name) }
    var actions by remember {
        mutableStateOf(mapOf(Press.SINGLE to mapping.actions, Press.DOUBLE to mapping.doublePress, Press.LONG to mapping.longPress))
    }
    var picking by remember { mutableStateOf<Pair<Press, Input>?>(null) }
    Dialog(onDismissRequest = onClose, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Page {
            TopBar(if (editing.new) "New mapping" else "Edit mapping", onClose)
            OutlinedTextField(
                name, { name = it },
                label = { Text("Name") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().padding(vertical = 8.dp),
            )
            for (press in Press.entries) {
                Heading(press.title)
                SectionCard(MaterialTheme.colorScheme.surfaceContainer) {
                    for (input in Input.entries) {
                        ListRow(
                            Leading.Glyph(UiR.drawable.ic_sl_remote), input.title,
                            (actions.getValue(press)[input] ?: Action.NONE).title,
                            trailing = { Chevron() },
                        ) { picking = press to input }
                    }
                }
            }
            Row(Modifier.padding(start = 16.dp, top = 12.dp)) {
                FilledButton("Save") {
                    // Nothing on a double or long press leaves the input acting as it's pressed.
                    fun gestures(press: Press) = actions.getValue(press).filterValues { it != Action.NONE }
                    onSave(
                        mapping.copy(
                            name = name.trim().ifEmpty { mapping.name },
                            actions = actions.getValue(Press.SINGLE),
                            doublePress = gestures(Press.DOUBLE),
                            longPress = gestures(Press.LONG),
                        ),
                    )
                }
                if (!editing.new) PlainButton("Delete", onDelete)
            }
        }
        picking?.let { (press, input) ->
            val options = Action.entries
            val current = actions.getValue(press)
            val title = if (press == Press.SINGLE) input.title else "${input.title}, ${press.title.lowercase()}"
            ChoiceDialog(title, options.map { it.title }, options.indexOf(current[input] ?: Action.NONE), { picking = null }) {
                actions = actions + (press to current + (input to options[it]))
                picking = null
            }
        }
    }
}
