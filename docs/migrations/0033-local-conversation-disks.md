# 0033 — Local conversation disks

The manager retires S3-dependent working environments at startup, before any
worker recovery or admission. This applies to conversations and standalone mission
runs. It uses the detachment and background retirement queue of
[ADR-0083](../adr/0083-local-conversation-disks-without-s3.md), with cause
`storage-migration`; history, attachments and published artifacts remain.

One transaction cancels legacy node moves, capacity relocations and backend
conversions using the existing startup cancellation functions. It removes their
saved destination hints and paid move-recovery markers. Ordinary same-node
execution fencing after a manager restart remains necessary. The transaction
then expires environments with any of these indications of a possible remote base:

- An S3 `node-backups` point, a run backup pointer or an acknowledged disk-storage
  pointer. A published point may already be installed even if its acknowledgement
  was lost.
- A shared publication with block references, or a disk grant that names remote
  publications. These cover interrupted publication bookkeeping and missing point
  records; the manager does not assume those bases are safely local.
- An owner node, recorded on the run, checkpoint or unreleased execution admission,
  that is already revoked. This includes a materialized admission whose checkpoint
  was never published.

Pending publication demand without a point does not establish a remote base.
Never-published environments on nodes that have not been revoked keep their
journals and sessions, and lose obsolete backup-error status. Already detached
and pending expirations use their existing retirement queue. No block is fetched.
The same criteria apply to environments retained in Trash. Conversations whose
permanent purge is already underway keep their existing cleanup.
The old execution checkpoint is captured by retirement, so its process/lease is
fenced before a new environment can start; active missions without a scheduled
wait resume in an empty environment.

The transaction writes `migration:local-conversation-disks:v1` in the existing
`kv` table only after every detachment succeeds. A failure rolls everything back.
Subsequent starts do not repeat the migration, even while physical retirement is
still retrying. Disk deletion on the node needs no publication or base-block read.
Missing local base blocks prevent mount rather than triggering a remote restore.

This change adds **no schema change**: `DATABASE_VERSION` remains 32, introduced
by [detached publication acknowledgements](0032-detached-publication-acknowledgements.md).
Managers using schema 31 cannot open this database. Managers supporting schema 32
can technically open it, but that is not a rollback of this destructive migration.
A rollback requires an operator database backup taken before the
update and the matching bucket. Only the last remotely published files can return.
The supervisor does not take this database backup.

Remote deletion and shared-block collection workers no longer run, including for
already queued deletions. No bucket data is erased. S3 settings and deployment
configuration are left for a later change.

## Note de version

Les disques de travail restent désormais uniquement sur leur node, sans copie S3.
La mise à jour supprime les fichiers de travail des conversations existantes dont
l’environnement dépend de S3, ainsi que ceux des nodes déjà révoquées. L’historique,
les pièces jointes et les artefacts sont conservés. Ces critères s’appliquent aussi
aux conversations dans la corbeille. **Poussez vos commits et
sauvegardez les fichiers à conserver avant de mettre à jour.** Les environnements
jamais publiés sur une node non révoquée restent utilisables.

Le bucket S3, ses identifiants, `storage-s3.json`, les variables `STORAGE_S3_*` et
les données de Garage ne sont pas effacés. L’exploitant peut les retirer lorsqu’il
n’envisage plus de retour arrière. Revenir à une ancienne version exige une
sauvegarde de la base de données prise par l’exploitant **avant la mise à jour**,
ainsi que le bucket correspondant ; les fichiers ne reviennent que dans l’état de
leur dernière publication.
