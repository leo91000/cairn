# Architecture decisions

Each ADR has a unique number. Link its full filename and subject when citing it.
Supersession notices at the top identify the affected part of a historical decision.
The remaining text records the decision at the time; use the linked operating guides
for current behavior.

## Index

| ADR | Decision |
| --- | --- |
| 0001 | [Préserver la reprise et les livrables partagés lors de l’archivage](0001-conversation-archives-and-shared-deliverables.md) |
| 0002 | [Déplacer une conversation par pause et reprise](0002-conversation-movement-by-pause-and-resume.md) |
| 0003 | [Conserver Firecracker et différer le GPU](0003-defer-gpu-until-direct-firecracker-support.md) |
| 0004 | [Disque local et sauvegarde incrémentale asynchrone](0004-local-disk-asynchronous-incremental-backup.md) |
| 0005 | [Suivre les blocs écrits avec dm-era dans l'invité](0005-guest-dm-era-write-tracking.md) |
| 0006 | [Cache disque transparent et archivage distinct](0006-transparent-disk-cache-and-separate-archives.md) |
| 0007 | [Disques à la demande et journal local durable](0007-demand-paged-disks-with-durable-local-journal.md) |
| 0008 | [Publication du disque courant sans archives](0008-current-disk-publication-without-archives.md) |
| 0009 | [Disques S3 obligatoires sur toutes les nodes](0009-s3-backed-disks-required-on-every-node.md) |
| 0010 | [Blocs S3 partagés et objets immuables](0010-shared-block-incarnations.md) |
| 0011 | [Journal local segmenté et transitions de protection confirmées](0011-segmented-local-journal.md) |
| 0012 | [Slots et budgets partagés par node](0012-shared-node-budgets.md) |
| 0013 | [Managed tools without the native Codex plugin catalog](0013-managed-tools-without-native-plugin-catalog.md) |
| 0014 | [Boot without unused physical-device probes](0014-boot-without-unused-device-probes.md) |
| 0015 | [Initialize empty Codex schemas in the image](0015-codex-schema-templates.md) |
| 0016 | [Réserver l'espace des écritures sans sérialiser leur I/O](0016-pending-write-headroom.md) |
| 0017 | [Partager les synchronisations du journal entre écritures en attente](0017-durable-journal-groups.md) |
| 0018 | [Transport disque direct et restitution de la RAM partagée](0018-direct-block-queues-and-shared-ram.md) |
| 0019 | [VM pré-démarrées avec Codex prêt](0019-booted-vms-with-ready-codex.md) |
| 0020 | [0020 — Bounded CPU-paused conversation retention](0020-paused-conversation-retention.md) |
| 0021 | [Final publication without blocking the next turn](0021-final-publication-without-blocking-the-next-turn.md) |
| 0022 | [Transport ublk optionnel et VMGenID](0022-optional-ublk-transport-and-vmgenid.md) |
| 0023 | [Sauvegardes périodiques sans gel du guest](0023-periodic-journal-capture-without-guest-freeze.md) |
| 0024 | [Templates anonymes avec mémoire privée partagée](0024-anonymous-snapshots-with-private-guest-memory.md) |
| 0025 | [Pool borné pour les arrivées simultanées](0025-bounded-pool-for-simultaneous-arrivals.md) |
| 0026 | [Codex : conserver WebSocket après comparaison HTTP](0026-codex-transport-first-output.md) |
| 0027 | [One journal reader for a stopped-disk publication transfer](0027-stopped-disk-publication-reader.md) |
| 0028 | [Installations autonomes, accessibles uniquement par le service officiel](0028-autonomous-installations-behind-the-official-service.md) |
| 0029 | [Partage d’une installation entière, sans conversations privées](0029-sharing-whole-installations.md) |
| 0030 | [Récupération avant la limite de RAM partagée, et reprise des VM tuées](0030-reclaim-before-the-shared-memory-limit.md) |
| 0031 | [Préchauffage borné et récupération des images runtime](0031-runtime-cache-and-warmup-failures.md) |
| 0032 | [Un seul processus officiel de relais avant coordination distribuée](0032-single-official-relay-process.md) |
| 0033 | [Connexion directe quand le réseau le permet, relais de secours sinon](0033-direct-connection-with-relay-fallback.md) |
| 0034 | [Cairn et Beacon avant la première release](0034-cairn-and-beacon-before-release.md) |
| 0035 | [Canal MCP local à la VM, relayé par vsock](0035-vm-local-mcp-channel.md) |
| 0036 | [Compression of immutable disk objects](0036-compressed-immutable-disk-objects.md) |
| 0037 | [Native execution behind coding-agent adapters](0037-coding-agent-execution-adapters.md) |
| 0038 | [Luau installation plugins and explicit external bot delegation](0038-luau-installation-plugins-and-external-bot-delegation.md) |
| 0039 | [Wait for required pull-request checks at an exact head](0039-required-pr-check-waits.md) |
| 0040 | [0040 — Durable exclusive node reservations](0040-exclusive-node-reservations.md) |
| 0041 | [Immediate permanent conversation deletion](0041-immediate-conversation-deletion.md) |
| 0042 | [OpenCode through native sessions and model-provider API keys](0042-opencode-api-key-integration.md) |
| 0043 | [Diagnostics de performances Android envoyés à Beacon par défaut](0043-client-performance-telemetry.md) |
| 0044 | [Public outbound UDP from agent microVMs](0044-public-outbound-udp.md) |
| 0045 | [Declarative plugin contributions and declared programs](0045-declarative-plugin-contributions.md) |
| 0046 | [Plugin transports and setup forms in Luau](0046-plugin-transports-and-forms-in-luau.md) |
| 0047 | [Multi-file plugin packages](0047-multi-file-plugin-packages.md) |
| 0048 | [Durable discard for current and retained guest images](0048-durable-discard-for-retained-guests.md) |
| 0049 | [Plugin-owned program installation and JSON/TOML manifests](0049-plugin-program-installers-and-toml-manifests.md) |
| 0050 | [Trusted native execution alongside Firecracker](0050-trusted-native-execution.md) |
| 0051 | [Toutes les permissions d’un agent se règlent sur sa page](0051-agent-centric-permissions.md) |
| 0052 | [Native workspaces use the VM journal through host ublk](0052-native-ublk-workspaces-and-idle-eviction.md) |
| 0053 | [GitHub devient un plugin livré avec Cairn](0053-github-as-a-bundled-plugin.md) |
| 0054 | [Existing conversations can explicitly migrate between Linux backends](0054-explicit-conversation-backend-migration.md) |
| 0055 | [ublk as the sole VM block transport](0055-ublk-only-vm-storage.md) |
| 0056 | [Generic plugin OAuth, private transfers and Gmail](0056-plugin-oauth-file-transfers-and-gmail.md) |
| 0057 | [Guided plugin setup and shared provider apps](0057-guided-plugin-setup.md) |
| 0058 | [Prepared native project bases use sealed journal segments](0058-prepared-native-project-bases.md) |
| 0059 | [Delegated tasks across coding agents and models](0059-delegated-tasks.md) |
| 0060 | [Grouped wait conditions](0060-grouped-wait-conditions.md) |
| 0061 | [Un seul pool de comptes pour tous les agents de code](0061-one-account-pool-for-coding-agents.md) |
| 0062 | [Verified block cache and active synchronization budget](0062-verified-block-cache-and-active-backup-budget.md) |
| 0063 | [Bounded recovery after transient agent failures](0063-bounded-agent-error-recovery.md) |
| 0064 | [Accès explicite d’un agent à l’installation courante](0064-opt-in-installation-tools-for-agents.md) |
| 0065 | [Durable conversation wakeups](0065-durable-conversation-wakeups.md) |
| 0066 | [Slugs lisibles pour les installations et les conversations](0066-readable-workspace-urls.md) |
| 0067 | [Durable conditions for conversation continuation](0067-durable-condition-waits.md) |
| 0068 | [Owner-chosen agents for bot delegation](0068-owner-chosen-agents-for-bot-delegation.md) |
| 0069 | [Nodes mises en relation par Beacon, sans adresse](0069-nodes-introduced-by-beacon.md) |
| 0070 | [Automatic bounded native disk growth](0070-automatic-native-disk-growth.md) |
| 0071 | [Managed plugin program caches](0071-managed-plugin-program-caches.md) |
| 0072 | [Fast journal recovery and receipt reconciliation](0072-fast-journal-recovery-and-receipt-reconciliation.md) |
| 0073 | [Automatic placement with per-agent node priorities](0073-automatic-agent-placement.md) |
| 0074 | [Un outil Cairn commun pour les questions des agents](0074-common-agent-question-tool.md) |
| 0075 | [Hooks de plugins, règles sans code et droits restreints par ressource](0075-plugin-hooks-rules-and-scoped-grants.md) |
| 0077 | [Native close with independent backend namespaces](0077-native-close-with-independent-backend-namespaces.md) |
| 0078 | [Side questions use Claude Code's native side mechanism; Codex stays unavailable](0078-native-side-questions.md) |

## Renumbered collisions

The duplicate filenames below were assigned unused numbers. No ADR was deleted.
Earlier links in issues and PRs can be resolved with this table.

| Former filename | Current decision |
| --- | --- |
| `0002-one-account-pool-for-coding-agents.md` | [ADR-0061](0061-one-account-pool-for-coding-agents.md) |
| `0012-verified-block-cache-and-active-backup-budget.md` | [ADR-0062](0062-verified-block-cache-and-active-backup-budget.md) |
| `0032-bounded-agent-error-recovery.md` | [ADR-0063](0063-bounded-agent-error-recovery.md) |
| `0036-opt-in-installation-tools-for-agents.md` | [ADR-0064](0064-opt-in-installation-tools-for-agents.md) |
| `0037-durable-conversation-wakeups.md` | [ADR-0065](0065-durable-conversation-wakeups.md) |
| `0037-readable-workspace-urls.md` | [ADR-0066](0066-readable-workspace-urls.md) |
| `0038-durable-condition-waits.md` | [ADR-0067](0067-durable-condition-waits.md) |
| `0059-owner-chosen-agents-for-bot-delegation.md` | [ADR-0068](0068-owner-chosen-agents-for-bot-delegation.md) |
| `0059-nodes-introduced-by-beacon.md` | [ADR-0069](0069-nodes-introduced-by-beacon.md) |
| `0060-automatic-native-disk-growth.md` | [ADR-0070](0070-automatic-native-disk-growth.md) |
