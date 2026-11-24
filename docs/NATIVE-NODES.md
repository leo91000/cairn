# Operating a native node

Native execution is for selected agents used by trusted people. It runs host GPU
APIs under a managed account while the owner continues using the computer. It
provides less isolation than Firecracker. GPU memory exhaustion, desktop slowdown
and driver bugs remain possible; no software boundary can promise a GPU job will
never crash a driver. Review code before testing on a personal workstation.

## Requirements and enrollment

Use Linux x86-64 with systemd 255+, cgroup v2, Python 3, curl, useradd/usermod,
e2fsprogs, rsync and a dedicated XFS or ext4 project-quota data pool. Protected
XFS launch uses idmapped mounts with a compatible kernel and util-linux 2.39
or later. A missing mount capability uses journaled host ownership.
Legacy journal conversion also needs io_uring and a working `ublk_drv` driver.
Host libraries must be
compatible with the published coding engines. The installed GPU
driver, render/video device permissions and application libraries must already
work. Cairn does not install GPU drivers, detach a GPU or close a graphical session.
Enrollment verifies the quota pool before starting or draining the controller.
It loads/checks ublk only when legacy journals or attachment records require it;
new directory environments do not require that driver.
The installation manager must be configured; conversation disks require no S3 storage.

As the installation owner, open Nodes, add a machine and choose Native process.
Use the generated installer on that node and enter the single-use code when asked.
Without a direct manager address, it first installs the node gateway, which Beacon
introduces to the manager ([additional execution nodes](DEPLOYMENT.md#additional-execution-nodes)).
The installer creates `cairn-workload`, installs a root-owned systemd controller,
and leaves native execution disabled. In Configure, explicitly enable native work,
select trusted people and conservative CPU/RAM/disk limits. Configure a native
agent and select this node. Other agents retain their Firecracker backend.

Approved updates drain the node before selecting the verified candidate. The
selected candidate is retained across failed health checks and service restarts.
A durable update record retries the manager's maintenance-completion
acknowledgement once the candidate is healthy, including after a lost reply.

Start a new conversation as a trusted person. An authorized coordinator can also
start a conversation with that native agent through its installation tools. Each
follow-up, resume, scheduled dispatch and delegated run retains and rechecks its
originating permission. Previously unattributed conversations and external plugin
origins cannot silently gain host access. Explicit node moves may change an
existing conversation’s backend under those same checks; see ADR-0082.

## Switching an existing machine

A node keeps the execution backend it enrolled with; a heartbeat reporting another
backend is refused. To move a Firecracker machine to native execution, or back, use
Switch to native (or Switch to VM) on its node. Cairn pre-fills the machine name
and the other backend. Run the generated installer on the same machine: the new
runtime enrolls as a separate node with its own identity, grants and storage,
alongside the existing one. After its configuration and agent step, Cairn offers to
retire the previous node. The invitation assigns the replacement's identity before
it connects, so another enrollment cannot advance this switch. Every invitation
and enrolling component must carry the current identity and version.
Revoking it from there also returns conversations fixed
to or preferring that node to automatic placement; a plain revocation keeps those
explicit choices waiting. Switch each agent's backend on its page and allow the
new node. Existing conversations show **Migrate this conversation…** in their
execution-node details on web and Android. Confirming discards the working files:
the environment expires and its first empty environment starts on the selected
node. History, attachments and artifacts remain. The current turn stops, questions
close, queued messages and waits keep their continuations, and active missions
resume automatically. A chat otherwise waits for its next message. There is no
disk transfer or physical conversion. Back up needed files before confirmation.
Finally run the uninstall command shown for the previous runtime.

The public Cairn MCP and the `cairn_installation` agent tool expose the same setup
and retirement controls: `list_nodes`, `list_native_callers`,
`create_node_enrollment`, `update_node`, `update_node_storage` and `revoke_node`. Installation management
access is required for mutations; read-only public MCP access cannot enroll,
configure or revoke a node. Agent installation grants are checked again in each
mutation transaction. Enroll the other backend, wait for the returned `nodeId`,
configure its native trust when applicable, and grant agents through `update_agent`.
Only after the replacement is ready, revoke the old identity with
`discardWorkingFiles: true` and `releasePlacements: true`. Plain revocation also
requires the discard confirmation but preserves existing placement choices.
Revocation expires every environment on the node; the next turn starts empty.
`get_chat_backend_migration` previews the saved/profile backend difference,
authorized destinations and the working-file loss warning. `migrate_chat_backend`
accepts either `chatId` or `runId`, plus `nodeId`, the destination `backend`,
`confirm: true` and, while the environment exists, `discardWorkingFiles: true`.
For an already expired environment the discard confirmation and warning are
unnecessary. These tools neither install nor uninstall a runtime on the host.

## Daily use

An agent can use all nodes, including future ones, or an explicit selection.
New conversations use its selected execution backend. Existing environments keep
their saved backend; after an agent changes backend, migrate its existing
conversations before resuming them. An all-node grant does not replace that
migration. Native execution still requires the node owner's opt-in, trust
in every originating person, and fresh membership and delegation evidence.

Native runtimes supporting exclusive reservations report `exclusiveReservations: true`.
The owner grants “Reserve exclusively” on the agent's node access page, on web or
Android. An authorized run uses `acquire_exclusive`, `exclusive_status`,
`renew_exclusive` and `release_exclusive` through its workspace tools. The same
agent profiles and node capabilities are exposed through public Cairn MCP and
`cairn_installation`; installation agents cannot grant themselves exclusivity.
Reservations block new Cairn execution and freeze other managed workload cgroups,
including descendants, with confirmed automatic resume after release or expiration.
Storage protection retains its own suspension reason. Confirmed exclusive pauses
extend execution time budgets; security leases keep expiring normally. Native
controller recovery fences the old workloads before clearing reservation ownership.

This reserves Cairn work on that node, not external applications, physical GPUs or
the whole host. Frozen processes keep their RAM and GPU allocations. Optional RAM
headroom is checked against real shared cgroup usage and host availability; native
reservations fail when it is unavailable rather than reclaiming peer memory.
Administrator enforcement remains cooperative because root can escape cgroups.
Older native runtimes require an update before the reservation can be used.

The controller defaults to one slot and supports configurable concurrent executions.
All slots share the configured CPU/RAM ceiling through a systemd workload slice,
and the existing disk budget. Per-conversation limits are ceilings, not reservations.
It uses low CPU/I/O priority and conservative default budgets.
Keep interactive applications running during qualification. Start with a short,
small GPU workload and inspect memory use and desktop responsiveness before larger
jobs. VRAM has no universal hard per-agent quota. Disabling native execution or
removing a trusted person stops affected work after its remaining lease expires.
Turning off admission prevents new placement without automatically canceling work.
An offline Beacon authority check eventually stops native work too.

The managed filesystem contains system binaries/libraries read-only, a private
HOME, project files, output, temporary files and GPU devices. The personal home,
desktop sockets, host Docker daemon and administrator privileges are unavailable.
Conversations use their own lazy [rootless Docker enclave](NATIVE-DOCKER.md) for
builds, containers and Compose. Published ports are private to that conversation's
network; GPU containers and privileged Docker features are unavailable. Images,
build cache and named volumes follow the durable environment's native storage.
Changing nodes or execution backend expires the environment, including its Docker
images, build cache and named volumes. Push images or export data that must survive.
Graphics tests must use headless/offscreen APIs. Cairn bundles the coding engines,
including Codex's `codex-code-mode-host` companion beside its executable, Node.js
and `gh`; other toolchains use this node's installed binaries/libraries or
user-managed tools under the private home. Respect repository version pins.

Skills are the run's frozen bundle, copied afresh for each attempt. Accounts and
MCP access use the usual scoped brokers. Credentials provided to an engine are
available to that engine. Do not assume the checkpoint exclusions detect secrets
written into arbitrary project files.

## Storage and recovery

Expiration retirement uses a durable `retirementId`, shared by all retries for
that detached environment. The controller stores completion receipts in
`state/disk-retirements/<run>/<retirementId>` outside the conversation disk and
checks them under its storage lock before deletion. Receipts must survive disk
recreation and controller restarts: delayed requests become no-ops. A manager
requires the matching receipt acknowledgement before admitting a new environment
of the same run on this node. Older controllers that ignore this identity must be
upgraded before that readmission. Support is advertised as
`diskRetirementProtocol: 2`; the manager checks it before issuing deletion as well
as validating the identity in the acknowledgement. An unqualified deletion must
explicitly declare permanent conversation deletion. A manager-owned run namespace
is never erased as native node staging, and its marker is excluded from remote
seeds.

Native environments keep ordinary retained directories on the dedicated XFS or ext4
project-quota pool at `state/native-data/<run>/`. Controller receipts and locks are
on the separate host filesystem at `state/native/<run>/`. Stopping execution fences
its consumers, flushes work and removes managed temporary files; retained workspace,
home, Docker images and volumes stay in place for resume. Expiration removes that
environment's directories and kernel quota limits after confirming all consumers
are gone. See [local disk operations](CONVERSATION-STORAGE-OPERATIONS.md) for
capacity, crash-prefix handling, format migration and intentionally retained metadata.

Root-private data is under `/var/lib/cairn-native`. Transfer staging is in `data/`;
new retained files are in `state/native-data/<run>/data/`, with managed temporary
files in `state/native-data/<run>/tmp/`. The controller socket is
`state/controller.sock`. Only legacy conversion temporarily opens the old journal
and its ublk filesystem. Do not remove ownership/active markers to force a writer.
Missing retained data fails explicitly rather than recreating the original seed.

CPU/RAM/process limits and read/write IO rates use per-execution and shared cgroups.
Project quotas count allocated storage, including managed tmp and Docker data, and
bound zero-length files with inode limits. Node capacity observations preserve the
physical free-space reserve. Administrator limits remain cooperative.

Generated runtime configuration, account credentials and frozen skills are bound
from per-attempt staging instead of being copied into persisted blocks. Provider
session files, user tools and workspace/output files stay in the retained
directory. This does not detect secrets deliberately written into arbitrary user
files. Native conversations keep a distinct identity and subordinate ID range on
their node. Changing nodes or backends starts an empty environment; existing
container stores are not converted or transferred.

## Updates and removal

Revoke the node in Cairn first, then stop/disable `cairn-native-node.service` on its
host. Removing `/opt/cairn-native` and `/var/lib/cairn-native` deletes binaries and
all retained local work; push commits and export files to keep before confirming revocation.
After confirmed consumer shutdown, retire any persistent units installed by
`migrate-pool` before running the Revoke command again. Use the migration
receipt at `/var/lib/cairn-native/native-pool-migration/state.json` to identify the
XFS UUID and, for image targets, the backing file. Inspect the pool unit with
`systemctl cat "$(systemd-escape --path --suffix=mount /var/lib/cairn-native/state/native-data)"`
and confirm that its `What=` references that UUID. For an image target, also
inspect `systemctl cat cairn-native-data-loop.service` and confirm that it
references the recorded image and UUID. Disable and stop the verified mount
unit first, then disable and stop the verified loop service if present. Check
with `findmnt` that the pool was unmounted and with `losetup --associated` and
the recorded image path that no loop still holds it. Remove those verified
unit files, then run `systemctl daemon-reload`. Keep any storage volume or
backing file you need to retain outside the directories slated for removal.
Operator-provisioned mount units are managed by the operator.

After confirmed consumer shutdown, unmount the operator-provisioned native-data
pool before removing its mountpoint. First confirm with `findmnt` that no other
conversation disk is still mounted below
`/var/lib/cairn-native` after a failed stop. If one remains, start
`sudo systemctl start cairn-native-node` so its recovery releases the disks it owns,
then retry. Removal must never walk into a live filesystem. Delete the directories
and `cairn-workload` account before removing the systemd unit and running
`systemctl daemon-reload`. The Revoke dialog shows one command that lists such mounts
and stops before deleting anything, including when mount enumeration fails. It keeps
the unit file until the directories and account are gone, and skips stopping a unit
that no longer exists, so it can be rerun after any failed step. When no Firecracker
node remains on the machine, it also removes the node gateway. The
installer leaves GPU drivers and the personal desktop session untouched.

Claude's broker and CLI use a separate writable transient secure store for
credentials. The full Claude user directory remains retained, including
root-level instructions/settings, arbitrary tool state, projects, sessions and
installed plugins. Cairn-generated account metadata and configuration are bound
from staging; generated Codex configuration and frozen Cairn skills retain read-only
bindings. Other user tools and model caches in the private home remain durable.

## Node privilege mode

Native protected remains the default. Node details on web and Android can switch
to Native administrator, with an informed owner preview and confirmation, then
back to protected without enrollment or loss of conversation files/history.
Administrator is real host access through unrestricted passwordless sudo, for an
already isolated machine/VM. Transitions stop managed executions before restart;
pending or unconfirmed failure blocks new placements. Returning to protected does
not clean/restore the machine, undo changes or establish a new trust boundary.
Root can read local credentials and defeat its own supervisor and cooperative
limits. See [privilege admission, APIs, recovery and limitations](NATIVE-ADMINISTRATOR.md).

VM nodes provide a local image file directly to the VMM's block interface and
use a single flat ext4 filesystem. Neither new backend needs ublk or FUSE. VM suspension and
live memory reclamation preserve the running VMM; memory snapshots and the paired
filesystem layout are no longer supported.


## Pool de répertoires locaux

Les nouveaux environnements utilisent des répertoires ordinaires dans
`/var/lib/cairn-native/state/native-data/<conversation UUID>/`. Ce pool doit être
un système de fichiers XFS ou ext4 dédié, monté avec `prjquota`, appartenant à root sans
écriture pour les autres comptes. Son périphérique doit différer de celui du
répertoire `state` : les quotas de données ne réservent pas les inodes du système
de fichiers, et un pool plein doit encore pouvoir écrire ses reçus de nettoyage.
Les historiques et artefacts conservés par le manager ne sont pas déplacés dans
ce pool. Les cgroups bornent CPU/RAM/processus et les débits IO, pas les octets
occupés. Le contrôleur vérifie aussi que les quotas sont effectivement actifs.

Pour une nouvelle installation, préparez un volume XFS dédié avec
`mkfs.xfs -m crc=1,reflink=1 -i projid32bit=1` sur une cible vide, puis montez-le
de façon persistante avec `prjquota` avant l’enrôlement. Un pool ext4 existant
reste utilisable avec une copie des bases projet. XFS avec reflink partage les
blocs de chaque base entre les environnements privés.

### Migrer un nœud ext4 vers XFS hors ligne

Sur le pool ext4, mettez à jour le nœud vers une version comprenant la commande
`cairn native-pool-migrate`. Relancez la commande d’installation native fournie
par le manager de cette release pour rafraîchir `/opt/cairn-native/host.py` sans
réenrôler le nœud. Redémarrez le superviseur lorsque le nœud est inactif et
vérifiez qu’il est opérationnel sur ext4 avant de préparer la migration.
Installez `xfsprogs` et `rsync` sur le nœud, et vérifiez votre sauvegarde des
données du pool et du répertoire `/var/lib/cairn-native/state`.

Choisissez une partition ou un volume logique inutilisé sur le nœud. Remplacez
`/dev/mapper/cairn-native-xfs` dans l’exemple par cette cible. Le premier appel
affiche le plan. Le second formate la cible, arrête le service s’il tourne, copie
et vérifie les environnements, puis configure le montage de façon persistante.
Exécutez-le depuis une session d’administration qui survit à l’arrêt du nœud.

```sh
sudo python3 /opt/cairn-native/host.py migrate-pool --device /dev/mapper/cairn-native-xfs --dry-run
sudo python3 /opt/cairn-native/host.py migrate-pool --device /dev/mapper/cairn-native-xfs --stop
```

Si vous utilisez un fichier image, remplacez `--device …` par
`--image /var/lib/cairn-native/native-data.xfs --size 200G`, avec une capacité
adaptée aux données et à l’espace physique libre. L’outil alloue toute la taille
demandée sur le système de fichiers hôte et vérifie que le loop accepte le
direct I/O avant de formater le pool. Il désactive le discard sur ce loop
pour préserver l’allocation du fichier. Pour un pool source déclaré dans `/etc/fstab`, installez
d’abord une unité `.mount` persistante équivalente et vérifiez le montage ext4.
L’image doit être allouée sur toute sa plage avec des blocs privés. L’outil
vérifie cette allocation et l’association de l’inode au loop avant le formatage.
Intégrez les éventuels drop-ins à cette unité avant d’arrêter le nœud. Le retour
à ext4 rétablit les liens de démarrage présents avant la migration.
La migration réserve au moins 64 Mio, plus 64 Kio par environnement, pour les
reçus de récupération sur les systèmes de fichiers du contrôleur. Un fichier
image sur le même système de fichiers doit laisser cette réserve disponible.
L’outil conserve les restrictions de montage `nosuid`, `nodev`,
`noexec` et `nosymfollow` du pool ext4.
Il refuse les attributs inode qu’il ne sait pas préserver, par exemple
`nodump`, et indique le fichier concerné avant la bascule.

Après interruption, relancez `migrate-pool` avec la même cible pour reprendre.
Une copie interrompue est recréée sur la cible avant la vérification. Le
contrôleur refuse la bascule tant que l’écriture de la copie et des quotas sur
le disque n’a pas été confirmée.
Le nœud reste arrêté à la fin. `rollback-pool` restaure l’ancien montage et ses
reçus. Le contrôleur attend l’autorisation explicite `activate-pool`, même après
un redémarrage de la machine. Après cette autorisation, la première ouverture
du pool XFS en écriture ferme la possibilité de basculer vers ext4, afin de préserver les
nouvelles écritures. Conservez le pool ext4 et la sauvegarde pendant la
vérification.

```sh
sudo python3 /opt/cairn-native/host.py activate-pool
sudo systemctl start cairn-native-node.service
findmnt --mountpoint /var/lib/cairn-native/state/native-data
```

Le runtime peut être distribué sur ext4 par un superviseur utilisant le
protocole de distribution 2. Avant le formatage et avant l’autorisation
d’activation, l’outil vérifie que `/opt/cairn-native/host.py` correspond à la
version de l’outil exécuté.

Une reprise rouvre l’arbre existant. Une source manquante ou inconnue est refusée,
sans recréation silencieuse. La compatibilité ublk n’est chargée que pour des
journaux ou périphériques historiques ; un environnement neuf ne l’utilise pas.
Après vérification et bascule durable, le nettoyage efface ces anciennes ressources.
Le retour à l’ancien format exige une sauvegarde antérieure à la conversion.

La fin d’un tour clôt ses producteurs et son temporaire géré, mais retient ses
fichiers de travail et données Docker privées. L’expiration selon la politique
configurée, ou la suppression explicite, efface cet environnement. Les reçus
permanents d’idempotence et les inodes de verrou restent nécessaires contre les
retries tardifs ; le compteur de projets ne réutilise pas d’identifiant. Le pool
partagé, les caches bornés et un pilote déjà chargé pour d’autres ressources ne
sont pas des ressources privées d’une conversation et ne sont pas démontés par
son expiration.

La qualification système s’exécute uniquement sur infrastructure jetable. Le
contrôle ciblé `ci.yaml` avec `ci-diagnostic=storage`, `validation-only=true` et
`backend-only=false` donne un retour de développement sur les quotas et images.
Il ne remplace pas les qualifications complètes `node-checks`, `native-runtime`,
VM/Firecracker, ni la validation du head final avant merge.
