package build.cairn.app.ui

import android.app.DatePickerDialog
import android.app.TimePickerDialog
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import build.cairn.app.data.*
import build.cairn.contracts.CreateHookRuleInput
import build.cairn.contracts.HookRuleCatalog
import build.cairn.contracts.HookRuleDefinition
import build.cairn.contracts.HookRuleTrigger
import build.cairn.contracts.ReplaceHookRuleInput
import build.cairn.contracts.RuleCondition
import build.cairn.contracts.RuleNode
import build.cairn.contracts.SchedulePreviewInput
import build.cairn.contracts.createHookRule
import build.cairn.contracts.previewSchedule
import build.cairn.contracts.replaceHookRule
import java.time.Instant
import java.time.LocalDate
import java.time.LocalDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.util.UUID
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*

@Composable
private fun DateAndTime(
    value: LocalDateTime,
    maximumDate: LocalDate? = null,
    changed: (LocalDateTime) -> Unit,
) {
    val context = LocalContext.current
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        TextButton(
            onClick = {
                val picker =
                    DatePickerDialog(
                        context,
                        { _, year, month, day ->
                            changed(
                                LocalDateTime.of(year, month + 1, day, value.hour, value.minute)
                            )
                        },
                        value.year,
                        value.monthValue - 1,
                        value.dayOfMonth,
                    )
                if (maximumDate != null) {
                    picker.datePicker.maxDate =
                        maxOf(maximumDate, value.toLocalDate())
                            .atStartOfDay(ZoneId.systemDefault())
                            .toInstant()
                            .toEpochMilli()
                    picker.updateDate(value.year, value.monthValue - 1, value.dayOfMonth)
                }
                picker.show()
            }
        ) {
            Text(value.toLocalDate().toString())
        }
        TextButton(
            onClick = {
                TimePickerDialog(
                        context,
                        { _, hour, minute -> changed(value.withHour(hour).withMinute(minute)) },
                        value.hour,
                        value.minute,
                        true,
                    )
                    .show()
            }
        ) {
            Text(value.toLocalTime().toString())
        }
    }
}

@Composable
internal fun ClockScheduleFields(
    initial: HookRuleTrigger.C,
    api: CairnApi? = null,
    changed: (HookRuleTrigger.C) -> Unit,
) {
    var recurring by rememberSaveable { mutableStateOf(initial.cron != null) }
    var cron by rememberSaveable { mutableStateOf(initial.cron ?: "0 9 * * *") }
    var timezone by rememberSaveable { mutableStateOf(initial.timezone) }
    val scope = rememberCoroutineScope()
    var previews by
        remember(recurring, cron, timezone, api) { mutableStateOf<List<String>>(emptyList()) }
    var previewError by remember(recurring, cron, timezone, api) { mutableStateOf("") }
    var previewing by remember(recurring, cron, timezone, api) { mutableStateOf(false) }
    var date by rememberSaveable {
        val zone = runCatching { ZoneId.of(timezone) }.getOrDefault(ZoneId.systemDefault())
        val value = initial.at?.let { Instant.ofEpochMilli(it).atZone(zone).toLocalDateTime() }
        mutableStateOf(
            (value ?: LocalDateTime.now(zone).plusDays(1).withHour(9).withMinute(0)).toString()
        )
    }
    val local = LocalDateTime.parse(date)
    fun update() {
        val zone = runCatching { ZoneId.of(timezone.trim()) }.getOrNull()
        val selected = LocalDateTime.parse(date).withSecond(0).withNano(0)
        val offset = zone?.rules?.getValidOffsets(selected)?.firstOrNull()
        val instant = if (recurring) null else offset?.let { selected.toInstant(it).toEpochMilli() }
        val draft = runCatching {
            HookRuleTrigger.C(
                at = instant,
                cron = cron.trim().takeIf { recurring },
                timezone = timezone.trim(),
            )
        }
            .getOrElse { HookRuleTrigger.C(timezone = timezone.trim()) }
        changed(draft)
    }
    Choice(
        "Fréquence",
        if (recurring) "cron" else "once",
        listOf("cron" to "Récurrente", "once" to "Date unique"),
    ) {
        recurring = it == "cron"
        update()
    }
    if (recurring)
        OutlinedTextField(
            cron,
            {
                cron = it
                update()
            },
            Modifier.fillMaxWidth(),
            label = { Text("Cron (cinq champs)") },
            singleLine = true,
        )
    else
        DateAndTime(local, maximumDate = LocalDate.of(9999, 12, 31)) {
            date = it.toString()
            update()
        }
    OutlinedTextField(
        timezone,
        {
            timezone = it
            update()
        },
        Modifier.fillMaxWidth(),
        label = { Text("Fuseau horaire (IANA)") },
        singleLine = true,
    )
    if (recurring && api != null) {
        OutlinedButton(
            enabled = !previewing && cron.isNotBlank() && timezone.isNotBlank(),
            onClick = {
                val input = SchedulePreviewInput(cron = cron.trim(), timezone = timezone.trim())
                scope.launch {
                    previewing = true
                    previewError = ""
                    previews = emptyList()
                    try {
                        val result = api.previewSchedule(input)
                        val zone = ZoneId.of(result.timezone)
                        val formatter = DateTimeFormatter.ofLocalizedDateTime(FormatStyle.MEDIUM)
                        previews =
                            result.occurrences.map {
                                formatter.format(Instant.ofEpochMilli(it).atZone(zone))
                            }
                    } catch (error: Exception) {
                        if (error is CancellationException) throw error
                        previewError = error.message.orEmpty()
                    } finally {
                        previewing = false
                    }
                }
            },
        ) {
            Text("Prévisualiser les prochaines dates")
        }
        if (previewError.isNotBlank()) Text(previewError)
        previews.forEach { Text(it) }
    }
}

@Composable
internal fun ScheduleAgentDialog(
    vm: CairnViewModel,
    state: Workspace,
    catalog: HookRuleCatalog,
    preferredAgent: String,
    dismiss: () -> Unit,
    created: (JsonObject) -> Unit,
) {
    val scope = rememberCoroutineScope()
    var name by rememberSaveable { mutableStateOf("") }
    var agent by rememberSaveable {
        mutableStateOf(
            catalog.agents.firstOrNull { it.agentId == preferredAgent }?.agentId
                ?: catalog.agents.first().agentId
        )
    }
    var project by rememberSaveable { mutableStateOf("") }
    var brief by rememberSaveable { mutableStateOf("") }
    var triggerJson by rememberSaveable {
        mutableStateOf(
            wireJson.encodeToString(
                HookRuleTrigger.C(cron = "0 9 * * *", timezone = ZoneId.systemDefault().id)
            )
        )
    }
    val trigger = wireJson.decodeFromString<HookRuleTrigger.C>(triggerJson)
    var error by remember { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    val creationId by rememberSaveable { mutableStateOf(UUID.randomUUID().toString()) }
    val allowedProjects = state.agents.find { it.id == agent }?.access?.projects
    val projects = state.projects.filter { allowedProjects == null || it.id in allowedProjects }
    LaunchedEffect(agent, projects) {
        if (projects.none { it.id == project }) project = ""
    }
    val ready =
        name.isNotBlank() &&
            brief.isNotBlank() &&
            (trigger.cron?.isNotBlank() == true || (trigger.at ?: 0) > System.currentTimeMillis())
    AlertDialog(
        onDismissRequest = { if (!busy) dismiss() },
        title = { Text("Programmer un agent") },
        text = {
            Column(
                Modifier.verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                OutlinedTextField(
                    name,
                    { name = it.take(100) },
                    Modifier.fillMaxWidth(),
                    label = { Text("Nom") },
                    singleLine = true,
                )
                Choice("Agent", agent, catalog.agents.map { it.agentId to it.name }) { agent = it }
                Choice(
                    "Projet",
                    project,
                    listOf("" to "Espace de l’agent") + projects.map { it.id to it.name },
                ) {
                    project = it
                }
                OutlinedTextField(
                    brief,
                    { brief = it },
                    Modifier.fillMaxWidth(),
                    label = { Text("Brief") },
                    minLines = 3,
                )
                ClockScheduleFields(trigger, vm.api) { triggerJson = wireJson.encodeToString(it) }
                if (error.isNotBlank()) Text(error)
            }
        },
        confirmButton = {
            Button(
                enabled = ready && !busy,
                onClick = {
                    scope.launch {
                        busy = true
                        error = ""
                        try {
                            val action =
                                ruleObject(
                                    "type" to JsonPrimitive("action"),
                                    "id" to JsonPrimitive("start_agent"),
                                    "action" to
                                        ruleObject(
                                            "type" to JsonPrimitive("startAgent"),
                                            "agentId" to JsonPrimitive(agent),
                                            "projectId" to
                                                project
                                                    .takeIf { it.isNotEmpty() }
                                                    ?.let(::JsonPrimitive),
                                            "prompt" to
                                                ruleObject(
                                                    "type" to JsonPrimitive("literal"),
                                                    "value" to JsonPrimitive(brief.trim()),
                                                ),
                                        ),
                                )
                            val definition =
                                HookRuleDefinition(
                                    executionAgentId = agent,
                                    limits = catalog.limits.defaultLimits,
                                    name = name.trim(),
                                    steps =
                                        listOf(
                                            RuleNode.A(
                                                RuleCondition.C(
                                                    ruleObject(
                                                        "type" to
                                                            JsonPrimitive("ruleConversationWorking")
                                                    )
                                                ),
                                                "avoid_overlap",
                                                then = listOf(action),
                                            )
                                        ),
                                    trigger = trigger,
                                )
                            var saved =
                                vm.api.createHookRule(CreateHookRuleInput(creationId, definition))
                            val storedDefinition =
                                wireJson.encodeToJsonElement(saved.definition).jsonObject
                            val desiredDefinition =
                                wireJson.encodeToJsonElement(definition).jsonObject
                            if (!sameRuleDefinition(storedDefinition, desiredDefinition)) {
                                check(saved.version == 1L) {
                                    "Cette programmation a été modifiée ailleurs. Ouvrez-la depuis les Règles."
                                }
                                saved =
                                    vm.api.replaceHookRule(
                                        ReplaceHookRuleInput(
                                            definition = definition,
                                            revision = saved.revision,
                                            ruleId = saved.id,
                                        )
                                    )
                            }
                            created(wireJson.encodeToJsonElement(saved).jsonObject)
                        } catch (e: Exception) {
                            if (e is CancellationException) throw e
                            error =
                                e.message?.takeIf { it.isNotBlank() }
                                    ?: "Impossible de confirmer la création. Réessayez."
                        } finally {
                            busy = false
                        }
                    }
                },
            ) {
                Text("Créer la programmation")
            }
        },
        dismissButton = { TextButton(onClick = dismiss, enabled = !busy) { Text("Annuler") } },
    )
}

@Composable
internal fun SnoozeConversationDialog(title: String, dismiss: () -> Unit, snooze: (Long) -> Unit) {
    val zone = ZoneId.systemDefault()
    var date by remember {
        mutableStateOf(Instant.ofEpochMilli(tomorrowMorning()).atZone(zone).toLocalDateTime())
    }
    val until =
        zone.rules.getValidOffsets(date).firstOrNull()?.let { date.toInstant(it).toEpochMilli() }
    AlertDialog(
        onDismissRequest = dismiss,
        title = { Text("Reporter la conversation") },
        text = {
            Column {
                Text(title)
                DateAndTime(date) { date = it }
            }
        },
        confirmButton = {
            Button(
                enabled = until != null && until > System.currentTimeMillis(),
                onClick = { until?.let(snooze) },
            ) {
                Text("Reporter")
            }
        },
        dismissButton = { TextButton(onClick = dismiss) { Text("Annuler") } },
    )
}
