# Sauvegarde incrémentale : suivre les blocs écrits de `data.ext4`

Recherche du 26 septembre 2026, sur sources primaires uniquement (docs officielles, code source à des tags figés). Les mesures et l'implémentation retenue figurent en fin de document. Complète [INCREMENTAL-VM-BACKUP-RESEARCH.md](INCREMENTAL-VM-BACKUP-RESEARCH.md). **(non vérifié)** marque ce qui n'a pas pu être confirmé par une source primaire ; « déduction » marque un raisonnement non documenté tel quel.

## Point de départ

Toutes les 60 s, Cairn gèle le FS invité, met la VM en pause, puis lance `cp --reflink=auto --sparse=always` ([checkpoint.rs](../crates/installation/src/nodes/checkpoint.rs)). Il relisait ensuite **toute** la copie par blocs de 4 Mio pour calculer un SHA-256, trous compris ; l'indexation saute désormais les trous avec `SEEK_DATA` ([snapshots.rs](../crates/installation/src/nodes/snapshots.rs)). Les disques sont configurés sans `io_engine` ni `cache_type`, donc `Sync` et `Unsafe` par défaut, et sans `discard`. Ils sont liés en dur dans le chroot du jailer ([host.rs](../crates/installation/src/microvm/host.rs)). Le noyau invité n'active ni device-mapper ni btrfs ([kernel.config](../deploy/microvm/kernel.config)).

## 1. Firecracker 1.17.0 lui-même

- **Aucun suivi des blocs disque modifiés.** `track_dirty_pages` s'appuie sur le dirty log KVM de la **mémoire** et ne sert qu'aux snapshots différentiels ([snapshot-support.md](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/docs/snapshotting/snapshot-support.md#creating-diff-snapshots), [swagger](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/src/firecracker/swagger/firecracker.yaml#L1467-L1474)). Les fichiers disque « doivent être sauvegardés par l'utilisateur » ; Firecracker les draine et les `fsync` lors d'un snapshot ([snapshot-support.md](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/docs/snapshotting/snapshot-support.md#creating-full-snapshots)). Dans le code bloc, « dirty » ne marque que la mémoire invitée ([async_io.rs](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/src/vmm/src/devices/virtio/block/virtio/io/async_io.rs#L56-L66)).
- **Moteurs** : `Sync` par défaut ; `Async` (io_uring) est en *developer preview* et « pas encore adapté à la production ». Il exige un hôte ≥ 5.10.51, et ses workers io_uring échappent au cgroup de la VM sur les noyaux 5.10 ([block-io-engine.md](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/docs/api_requests/block-io-engine.md)).
- **`path_on_host` peut être un périphérique bloc hôte.** La doc indique « backed by a file (or a block device) » ([block.md](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/docs/block.md#firecracker-virtio-block)). Le code ouvre le chemin sans `O_DIRECT`, mesure la taille par `seek(End)` et interroge `BLKSSZGET/BLKPBSZGET/BLKIOMIN/BLKIOOPT` si c'est un bloc ([device.rs](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/src/vmm/src/devices/virtio/block/virtio/device.rs#L70-L90), [L177-L207](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/src/vmm/src/devices/virtio/block/virtio/device.rs#L177-L207)). Ces ioctl et `BLKDISCARD` sont autorisés par le seccomp par défaut ([seccomp x86_64](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/resources/seccomp/x86_64-unknown-linux-musl.json#L589-L773)). `/dev/mapper/x`, `/dev/loopN`, `/dev/nbdN` ou `/dev/ublkbN` sont donc acceptables en principe ; aucun n'est cité nommément.
- **Jailer** : l'utilisateur doit lier en dur ou copier les ressources dans le chroot ([jailer.md](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/docs/jailer.md#observations)). Le jailer crée lui-même par `mknod` seulement `/dev/kvm` et `/dev/net/tun`. Pour un périphérique bloc, il faudrait créer un nœud `mknod` dans le chroot avec le bon propriétaire. Déduction, non documentée par Firecracker (le lien dur ne traverse pas les systèmes de fichiers). **(non vérifié)** : effet d'une éventuelle option `nodev` sur le montage du chroot.

## 2. vhost-user-block

- **Statut 1.17.0** : *developer preview*. Snapshots Firecracker impossibles avec un tel périphérique. Pas de rate limiting côté Firecracker. La mémoire invitée passe en `memfd` `MAP_SHARED`, avec des défauts de page jusqu'à 24 % plus lents d'après leurs mesures. Pas de page cache hôte. Pas de discard. Pas de drive `readonly` côté Firecracker. Le jailer ne sait pas lancer le backend ([block-vhost-user.md](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/docs/api_requests/block-vhost-user.md), [block-discard.md](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/docs/api_requests/block-discard.md#supported-configuration)).
- **Backends Rust extensibles** : Firecracker cite QEMU contrib, Cloud Hypervisor, crosvm et SPDK. Le backend Cloud Hypervisor ouvre un fichier raw (option `direct` → `O_DIRECT`) et ne suit pas les écritures ; il faudrait y ajouter un bitmap ([lib.rs](https://github.com/cloud-hypervisor/cloud-hypervisor/blob/6be17bc8bd19034ca99438a8cd5aa710f445b8af/vhost_user_block/src/lib.rs#L216-L240)). Il repose sur le crate rust-vmm `vhost-user-backend` (dernier tag : [v0.23.0](https://github.com/rust-vmm/vhost/tree/vhost-user-backend-v0.23.0/vhost-user-backend)).
- **qemu-storage-daemon (QSD)** sait exporter un nœud en `vhost-user-blk`, en `nbd` avec `bitmap=` et en `fuse` (un fichier régulier) ([qemu-storage-daemon.rst v11.1.1](https://gitlab.com/qemu-project/qemu/-/blob/v11.1.1/docs/tools/qemu-storage-daemon.rst)). Un bitmap créé par QMP `block-dirty-bitmap-add` suit les écritures du nœud. Il est **persistant uniquement sur qcow2** ; sur raw, il est perdu à la sortie ([bitmaps.rst](https://gitlab.com/qemu-project/qemu/-/blob/v11.1.1/docs/interop/bitmaps.rst#supported-image-formats)). `query-block` ne donne que le compte d'octets sales ; la liste des plages se lit par NBD via le contexte `qemu:dirty-bitmap:NOM` ([block-export.json](https://gitlab.com/qemu-project/qemu/-/blob/v11.1.1/qapi/block-export.json#L104-L119)). Les transactions QMP permettent un reset atomique du bitmap avec une sauvegarde ([bitmaps.rst § Transactions](https://gitlab.com/qemu-project/qemu/-/blob/v11.1.1/docs/interop/bitmaps.rst#transactions)). Un bitmap marqué `inconsistent` après un arrêt brutal devient inutilisable, d'où une sauvegarde complète. **(non vérifié)** : compatibilité réelle entre le frontend vhost-user de Firecracker et l'export vhost-user-blk de QSD ; Firecracker ne cite que `contrib/vhost-user-blk`.

## 3. dm-era côté hôte

- **Mécanisme** : cible linéaire qui enregistre les blocs écrits pendant une « ère ». Syntaxe : `era <metadata dev> <origin dev> <block size>`. Messages `checkpoint` (passe *éventuellement* à l'ère suivante, relire le statut), `take_metadata_snap` et `drop_metadata_snap`. Le statut donne l'ère courante. Les métadonnées sont écrites **avant** la première écriture d'un bloc non encore marqué : la doc affirme la résistance aux coupures de courant ([era.rst v6.12.109](https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/tree/Documentation/admin-guide/device-mapper/era.rst?h=v6.12.109), [process_deferred_bios](https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/tree/drivers/md/dm-era-target.c?h=v6.12.109#n1270)). Usage prévu : « tracking changed blocks for backup software ».
- **Code** : le compteur d'ère est sur 32 bits. `take_metadata_snap` fait lui-même un rollover d'ère puis un commit, et refuse un second snapshot tant que le premier n'est pas relâché ([dm-era-target.c#n1034](https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/tree/drivers/md/dm-era-target.c?h=v6.12.109#n1034)). La taille de bloc s'exprime en secteurs, multiple de 8 (4 Kio minimum) ([#n23](https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/tree/drivers/md/dm-era-target.c?h=v6.12.109#n23), [#n1462](https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/tree/drivers/md/dm-era-target.c?h=v6.12.109#n1462)). Les bios sont découpés à la taille de bloc (`dm_set_target_max_io_len`). Toute bio d'écriture non-flush marque son bloc, discard compris ([era_map](https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/tree/drivers/md/dm-era-target.c?h=v6.12.109#n1568)). Mémoire : `4 × nr_blocks` octets plus tampons. Kconfig : « Era target (EXPERIMENTAL) » ([Kconfig#n345](https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/tree/drivers/md/Kconfig?h=v6.12.109#n345)).
- **Outil** : `era_invalidate --written-since <ère> [--metadata-snapshot] <meta>` produit une liste XML `<blocks><range begin end/>` des blocs *pouvant* avoir changé. Sans snapshot, l'outil ne peut pas lire des métadonnées actives ([man era_invalidate, thin-provisioning-tools v1.3.4](https://github.com/jthornber/thin-provisioning-tools/blob/v1.3.4/man8/era_invalidate.txt), [invalidate.rs](https://github.com/jthornber/thin-provisioning-tools/blob/v1.3.4/src/era/invalidate.rs#L125-L173)).
- **Disponibilité** : `CONFIG_DM_ERA=m` chez Debian ([config Debian](https://salsa.debian.org/kernel-team/linux/-/blob/debian/latest/debian/config/config)) et Fedora ([config Fedora](https://src.fedoraproject.org/rpms/kernel/blob/rawhide/f/kernel-x86_64-fedora.config)). Chez Ubuntu 24.04, `dm-era.ko.zst` figure dans le paquet de base `linux-modules` ([liste 6.8.0-31](https://packages.ubuntu.com/noble/amd64/linux-modules-6.8.0-31-generic/filelist)).
- **Pour Cairn** : il faut `data.ext4` en `losetup`, un fichier de métadonnées également en loop, `dmsetup create` (root), puis un `mknod` du nœud dm dans le chroot. Ces loop/dm sont à nettoyer à chaque arrêt. Déplacement : les métadonnées ne valent que si le fichier de métadonnées accompagne `data.ext4` ; toute écriture hors dm-era (outil hôte, redimensionnement) invalide le suivi. Déduction : prévoir une sauvegarde complète après tout déplacement ou doute.

## 4. dm-era dans l'invité (noyau 6.12.109 maîtrisé)

- **Config** : `CONFIG_MD=y`, `CONFIG_BLK_DEV_DM=y` et `CONFIG_DM_ERA=y`. Ce dernier sélectionne `DM_PERSISTENT_DATA` (qui tire `LIBCRC32C` et `DM_BUFIO`) et `DM_BIO_PRISON` ([md/Kconfig](https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/tree/drivers/md/Kconfig?h=v6.12.109#n196), [persistent-data/Kconfig](https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/tree/drivers/md/persistent-data/Kconfig?h=v6.12.109)). En espace utilisateur invité : `dmsetup` et `era_invalidate`.
- **Topologie proposée** : un second petit disque virtio `era.meta`, lié en dur comme `data.ext4`, sans privilège hôte ni changement du jailer. L'init invité crée `era /dev/vdc /dev/vdb 8192` (4 Mio, aligné sur le manifeste actuel) et monte ext4 sur `/dev/mapper/data`. La table couvre tout `/dev/vdb` depuis le secteur 0 et `remap_to_origin` conserve le secteur ([#n1236](https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/tree/drivers/md/dm-era-target.c?h=v6.12.109#n1236)). Le bloc *b* correspond donc aux octets `[b×4 Mio, (b+1)×4 Mio)` de `data.ext4`. Mettre les métadonnées dans une partition de `data.ext4` casserait ce 1:1 et le format actuel (déduction).
- **Cycle proposé** (déduction) : `fsfreeze` → `take_metadata_snap` → `era_invalidate --metadata-snapshot --written-since N` → liste envoyée par vsock → l'hôte lit **seulement** ces plages de `data.ext4` pendant le gel/la pause → `drop_metadata_snap` → dégel. L'ère *N* suivante est lue dans le statut. Tant que le FS est gelé, `/dev/vdb` ne reçoit plus d'écritures. Avec le moteur `Sync`, les écritures déjà acquittées sont dans le page cache hôte, où la lecture par l'hôte les voit (déduction). La cohérence obtenue est au mieux celle d'une panne propre, comme aujourd'hui.
- **Pièges** : l'ancien disque sans métadonnées demande une sauvegarde complète initiale. Toute écriture sur `vdb` avant l'activation de la cible échappe au suivi (fsck lancé trop tôt, montage direct en secours, outils hôte) ; il faut une sauvegarde complète si l'ère ou le superbloc des métadonnées ne correspond pas. Taille des métadonnées sur disque non documentée **(non vérifié)** ; en RAM, 8192 blocs de 4 Mio donnent environ 32 Kio. Des blocs plus petits rendent le suivi plus fin, mais découpent davantage les I/O. `fsfreeze` n'agit que sur le FS : le disque de métadonnées continue d'être écrit par dm-era, c'est attendu. **Confiance** : l'invité exécute du code d'agent. Un invité malveillant ou cassé peut omettre des blocs et ne corrompre que sa propre sauvegarde, d'où une vérification complète périodique (déduction).

## 5. ublk ou NBD hôte avec serveur qui enregistre les écritures

- **ublk** : `CONFIG_BLK_DEV_UBLK`, marqué « Experimental » ; interface utilisateur « pas finalisée » ([drivers/block/Kconfig#n382](https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/tree/drivers/block/Kconfig?h=v6.12.109#n382)). Chaque I/O de `/dev/ublkbN` part vers le serveur par io_uring passthrough. Il existe un mode non privilégié (`UBLK_F_UNPRIVILEGED_DEV`) et une reprise après crash du serveur (`UBLK_F_USER_RECOVERY[_REISSUE]`) ([ublk.rst](https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/tree/Documentation/block/ublk.rst?h=v6.12.109)). Ubuntu place `ublk_drv` dans `linux-modules-extra`, pas dans le paquet de base ([liste](https://packages.ubuntu.com/noble/amd64/linux-modules-extra-6.8.0-31-generic/filelist)).
- **NBD** : `nbd.ko` est présent chez Debian, Fedora et Ubuntu (mêmes sources). `nbdkit-log-filter` journalise `Write`/`Trim`/`Zero` avec `offset` et `count`, dans un fichier ou via `logscript` ([nbdkit-log-filter](https://libguestfs.org/nbdkit-log-filter.1.html)). C'est un journal, pas un bitmap persistant, sans garantie de durabilité documentée. L'alternative est QSD avec un export NBD `bitmap=` (§2).
- **Pour Cairn** : il faudrait un démon par VM à superviser, une commande root pour attacher `/dev/nbdN`, et un `mknod` dans le jail. Un double passage en espace utilisateur (Firecracker → noyau → serveur) s'ajoute au chemin d'I/O. La persistance du bitmap et son ordre par rapport aux écritures restent à construire soi-même. Aucun chiffre de performance primaire pour cette combinaison.

## 6. Fichier FUSE comme `path_on_host`

- Firecracker n'utilise pas `O_DIRECT` (§1). Le moteur `Sync` fait des `pread`/`pwrite` ordinaires, donc un fichier FUSE marcherait en principe (déduction). **(non vérifié)** : moteur `Async`/io_uring sur FUSE. La visibilité du montage FUSE dans le chroot/mount namespace du jailer demanderait un bind mount, puisque le lien dur est impossible entre systèmes de fichiers.
- **Le passthrough FUSE (≥ 6.9) supprime justement la visibilité des écritures.** `read_iter`, `write_iter`, splice et mmap vont directement au fichier de support, sans le serveur ([passthrough.c](https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/tree/fs/fuse/passthrough.c?h=v6.12.109#n28), absent en [v6.8](https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/fs/fuse/passthrough.c?h=v6.8), présent en [v6.9](https://git.kernel.org/pub/scm/linux/kernel/git/torvalds/linux.git/plain/fs/fuse/passthrough.c?h=v6.9)). Le passthrough exige aussi `CAP_SYS_ADMIN` ([#n222](https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/tree/fs/fuse/passthrough.c?h=v6.12.109#n222)). Pour tracer, il faut le mode write-through ou direct-io, où chaque écriture devient une requête `WRITE` vers le démon ([fuse-io.rst](https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/tree/Documentation/filesystems/fuse-io.rst?h=v6.12.109)). Coût non chiffré par une source primaire.
- Variante prête à l'emploi : l'export `fuse` de QSD sur un qcow2 avec bitmap persistant (§2). Combinaison Firecracker + QSD FUSE **(non vérifiée)**.

## 7. Niveau système de fichiers

- **btrfs send -p dans l'invité** : flux d'instructions entre deux snapshots de sous-volume en lecture seule, consommé par `btrfs receive` ([btrfs-send](https://btrfs.readthedocs.io/en/latest/btrfs-send.html)). Le flux est au niveau fichier, pas bloc : il changerait le format de sauvegarde, la restauration (il faut un btrfs récepteur ou un stockage de flux chaînés) et le FS de `/run/data`, et demande `CONFIG_BTRFS_FS` dans l'invité. Rupture forte avec le manifeste par blocs de 4 Mio.
- **Reflink hôte + FIEMAP** : XFS (reflink activé par défaut par `mkfs.xfs`) et btrfs implémentent `remap_file_range` ; ext4 non ([mkfs.xfs(8)](https://man7.org/linux/man-pages/man8/mkfs.xfs.8.html), [xfs_file.c](https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/tree/fs/xfs/xfs_file.c?h=v6.12.109), [btrfs/file.c](https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/tree/fs/btrfs/file.c?h=v6.12.109), [ext4/file.c](https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/tree/fs/ext4/file.c?h=v6.12.109), [ioctl_ficlone(2)](https://man7.org/linux/man-pages/man2/ioctl_ficlone.2.html)). Sur ext4, `--reflink=auto` retombe sur une copie standard ([cp](https://www.gnu.org/software/coreutils/manual/html_node/cp-invocation.html)). Sur XFS, une écriture sur un bloc partagé est redirigée vers un nouveau bloc et le mapping logique→physique du fichier change ([mkfs.xfs(8)](https://man7.org/linux/man-pages/man8/mkfs.xfs.8.html)). Comparer `fe_logical`/`fe_physical` entre le clone N et le clone N+1 donne donc les extents réécrits sans relire les données (déduction). `FIEMAP_EXTENT_SHARED` signifie seulement « partagé avec d'autres fichiers » ([fiemap.h](https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/tree/include/uapi/linux/fiemap.h?h=v6.12.109#n67)), sans dire *avec quel fichier*. Ce drapeau n'est pas décrit dans [fiemap.rst](https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/tree/Documentation/filesystems/fiemap.rst?h=v6.12.109) et ne suffit pas si d'autres clones existent. Préférer la comparaison des adresses physiques. **(non vérifié)** : granularité CoW réelle (le `cowextsize` de XFS peut marquer des voisins), stabilité des adresses btrfs en cas de balance ou défragmentation (déduction : défrag et dédup cassent la comparaison, d'où une sauvegarde complète).

## 8. Gains immédiats sur le design actuel

- **Sauter les trous** : `SEEK_DATA`/`SEEK_HOLE` sont supportés par ext4 (≥ 3.8), XFS, btrfs, tmpfs et FUSE (≥ 4.5) ([lseek(2)](https://man7.org/linux/man-pages/man2/lseek.2.html)). `index()` lit aujourd'hui tous les octets, trous compris. Chercher les extents de données et poser directement `hash: null` pour les blocs de 4 Mio entièrement dans un trou supprime ces lectures. `cp --sparse=always` crée des trous pour toute longue suite de zéros de la source ([cp](https://www.gnu.org/software/coreutils/manual/html_node/cp-invocation.html)) ; coreutils utilise `SEEK_HOLE` à la lecture ([NEWS](https://git.savannah.gnu.org/cgit/coreutils.git/tree/NEWS)). La copie reste une relecture de toutes les données allouées.
- **Discard/TRIM** : Firecracker 1.17.0 ajoute `discard: true`, réservé aux drives inscriptibles en moteur `Sync`. Il fait un `fallocate(PUNCH_HOLE|KEEP_SIZE)` sur un fichier et `BLKDISCARD` sur un bloc ([CHANGELOG #6142](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/CHANGELOG.md#1170), [block-discard.md](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/docs/api_requests/block-discard.md), [sync_io.rs](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/src/vmm/src/devices/virtio/block/virtio/io/sync_io.rs#L39-L78)). Le pilote virtio_blk 6.12 gère `VIRTIO_BLK_F_DISCARD` ([virtio_blk.c](https://git.kernel.org/pub/scm/linux/kernel/git/stable/linux.git/tree/drivers/block/virtio_blk.c?h=v6.12.109#n1313)). Un `fstrim` périodique dans l'invité remet les blocs libérés en trous, qui disparaissent ensuite du travail de copie et de hachage. Compatible avec dm-era, où un discard marque le bloc.

## Comparatif

| Option | Blocs écrits seuls ? | Maturité | Privilèges / changements | Cohérence | Survit au déplacement | Effort |
| --- | --- | --- | --- | --- | --- | --- |
| 1. Firecracker natif | Non (mémoire seulement) | GA | — | — | — | — |
| 2a. Backend vhost-user maison + bitmap | Oui | Dev preview FC ; snapshots FC impossibles | Démon par VM, jail séparé | À construire | Si bitmap persisté | Élevé |
| 2b. QSD + bitmap qcow2 | Oui (NBD `qemu:dirty-bitmap`) | QEMU mûr ; combo FC non vérifié | qcow2, QSD par VM | Transactions QMP | Oui (qcow2) | Moyen-élevé |
| 3. dm-era hôte | Oui (≥ 4 Kio) | Kernel « EXPERIMENTAL », en distro | root, loop + dm, `mknod` jail | freeze + `take_metadata_snap` | Si métadonnées déplacées | Moyen |
| 4. dm-era invité | Oui (≥ 4 Kio) | Idem | Config noyau invité, 2e disque | freeze + `take_metadata_snap` | Oui (2 fichiers) | Moyen |
| 5. ublk/NBD + serveur | Oui | ublk expérimental | root, démon, `mknod` jail | À construire | À construire | Élevé |
| 6. FUSE traçant | Oui (sans passthrough) | FUSE mûr | Démon, montage visible du jail | À construire | À construire | Élevé |
| 7a. btrfs send invité | Oui, niveau fichier | Mûr | Change FS et format | Snapshot RO | Oui | Élevé |
| 7b. Reflink + FIEMAP hôte | Oui (extents) | Mûr | Hôte XFS/btrfs seulement | Pause + clone | Base complète après déplacement | Faible |
| 8. SEEK_DATA + discard | Non (ignore seulement les trous) | GA | `discard: true`, `fstrim` | Inchangée | Oui | Très faible |

## Recommandation classée pour Cairn

1. **Tout de suite (§8)** : `index()` fondé sur `SEEK_DATA`/`SEEK_HOLE`, `discard: true` sur `data` et `fstrim` invité régulier. Risque minimal, sans changement de format.
2. **Cible principale (§4) : dm-era dans l'invité** avec métadonnées sur un petit second disque et blocs de 4 Mio alignés sur le manifeste. Aucune dépendance au FS hôte (ext4 compris), aucun privilège hôte supplémentaire, jailer inchangé. Les métadonnées voyagent avec le disque. Garde-fous : sauvegarde complète au premier démarrage, après tout écart d'ère ou de métadonnées et après restauration ; comparaison complète périodique contre un invité menteur.
3. **Opportuniste (§7b)** : sur les hôtes XFS/btrfs, comparer les extents physiques du clone reflink précédent, sans modifier l'invité ; utile aussi pour valider §4.
4. **Seulement en cas de besoin de contrôle côté hôte (§3)** : dm-era hôte, qui impose root, loop/dm et `mknod` dans le jail.
5. **À éviter pour l'instant** : vhost-user/QSD (§2 : preview, perte des snapshots FC, qcow2), ublk/NBD (§5), FUSE (§6) et btrfs send (§7a). Tous sont trop lourds ou changent le format.

À mesurer avant validation : durée du gel et de la pause avec lecture ciblée, taille réelle des métadonnées dm-era sur 32 Gio, coût des I/O découpées à la taille de bloc, et restauration complète sur une autre node.

## Mesures du 26 septembre 2026 (prototype jetable)

Machine d'essai : 2 vCPU, KVM, hôte ext4 (overlay), Firecracker 1.17.0. Les chiffres sont indicatifs, pas des garanties.

**Coût de l'approche actuelle**, sur une image creuse de 32 Gio contenant 1,5 Gio de données réparties :

| Étape | Durée | Lu |
| --- | --- | --- |
| `cp --reflink=auto --sparse=always` (VM en pause pendant cette étape) | 5,6 s | 1,5 Gio |
| Indexation actuelle (`index()`, tous les blocs) | 32,4 s | 32 Gio |
| Indexation avec `SEEK_DATA` (extents alloués seulement) | 8,0 s | 1,5 Gio |
| Lecture des seuls blocs modifiés (20 blocs) | 0,5 s | 80 Mio |

Sur un hôte ext4, la copie sans reflink rend la pause proportionnelle aux données allouées, et pas à ce qui a changé.

**dm-era dans l'invité** : noyau 6.12.109 construit avec la config du projet plus `CONFIG_MD`, `CONFIG_BLK_DEV_DM` et `CONFIG_DM_ERA`. La cible `era /dev/vdc /dev/vdb 8192` est placée sur un disque de 32 Gio, avec un disque de métadonnées de 64 Mio. Premier démarrage : mkfs, puis 300 Mio et 300 fichiers. Second démarrage : un « tour d'agent » (fichiers modifiés, 20 Mio de sortie de build, suppression, 100 petits objets git). L'hôte compare ensuite les SHA-256 de chaque bloc de 4 Mio avant et après.

- 3 passes sur 3 : **11 blocs réellement modifiés, 11 signalés, 0 manqué, 0 en trop**. La liste prise pendant le gel est incluse dans la liste finale, qui couvre aussi le démontage.
- Du gel au dégel, avec `checkpoint`, `take_metadata_snap`, `era_invalidate` et `drop_metadata_snap` : **145 à 172 ms** pour un disque de 32 Gio.
- Écriture séquentielle de 512 Mio en `O_DIRECT` : 1,0 à 1,8 s en direct, 1,3 à 1,9 s via dm-era. 20 000 écritures synchrones de 4 Kio : environ 7,5 à 8,2 s dans les deux cas. Aucun surcoût mesurable au-delà du bruit de cette machine.
- Pièges rencontrés : l'invité n'a pas udev, donc il faut `dmsetup create --noudevsync` puis `dmsetup mknodes`. `era_invalidate` écrit `<range begin="110" end = "115"/>` avec des espaces autour de `=` et une fin exclusive. Un analyseur trop strict fait croire à des blocs manqués.

Scripts du prototype (hors dépôt) : init invité, harnais à deux démarrages, comparaison SHA-256 et banc d'indexation.

## Implémentation (27 septembre 2026)

Décision : [ADR 0005](adr/0005-guest-dm-era-write-tracking.md). Code : [tracking.rs](../crates/installation/src/nodes/tracking.rs) (état côté node), [checkpoint.rs](../crates/installation/src/nodes/checkpoint.rs) (capture), [era.rs](../crates/installation/src/microvm/era.rs) (invité), [init](../deploy/microvm/init).

**État conservé à côté de `data.ext4`.**
- `era.meta` : les métadonnées dm-era. L'invité les recharge à chaque démarrage, ce qui récupère aussi les écritures d'une VM arrêtée brutalement.
- `tracking/<instantané>.json` : l'ère et le manifeste des trois dernières captures.
- `sealed.json` : l'ère archivée par un arrêt propre.

**Captures.**
- Le master envoie l'identifiant d'instantané de son dernier point publié. Si la node en connaît la référence, seuls les blocs écrits depuis sont copiés, et le manifeste repart de celui de la référence.
- Capture active : l'invité gelé répond à `written` (`checkpoint`, `take_metadata_snap`, `era_invalidate`) **avant** la pause des vCPU. Une VM en pause ne peut pas répondre, et le FS gelé garde la liste exacte jusqu'à la fin de la copie.
- Disque arrêté : un arrêt propre a gelé le FS et archivé l'ère. L'hôte lit alors les métadonnées inactives avec `era_invalidate`, sans démarrer la VM.

**Invariant.** Toute écriture sur `data.ext4` passe par la cible dm-era de l'invité. Les cas suivants appellent `tracking::invalidate` avant d'agir, et la capture suivante est alors complète :
- création du disque ;
- `resize2fs` ;
- import d'une archive ;
- restauration d'un point de reprise ;
- un démarrage où l'invité signale l'absence de suivi (anciennes images, cible en échec).

**Garde-fous.**
- Une copie complète au moins toutes les 24 h.
- Copie complète aussi en cas de liste invalide, de taille de disque changée ou de référence inconnue.
- Une capture ratée efface la référence côté master.
- Un démarrage efface le scellé, qui ne décrit alors plus le disque.

**Validation.**
- De bout en bout, avec le vrai script de démarrage et le vrai `cairn guest` dans Firecracker 1.17 piloté par vsock, sur un même disque :
  - premier démarrage : écritures puis arrêt propre (ère scellée, VM arrêtée en 0,1 s). Lecture hors ligne par l'hôte : **9 blocs modifiés, 9 signalés** ;
  - deuxième démarrage tué par `SIGKILL` ;
  - troisième démarrage : la liste depuis l'ère scellée contient les écritures du démarrage tué, **5 sur 5**.
- Aucun bloc manqué dans aucune passe.
- La CI installe `thin-provisioning-tools` et teste la lecture hors ligne sur une vraie fixture de métadonnées. Le smoke test KVM vérifie une capture incrémentale sur l'image construite.

**Correction d'une conclusion précédente.** Après un arrêt brutal, lire directement les métadonnées sans recharger la cible ne montre pas l'ensemble d'écritures de l'ère en cours (0 bloc sur 9). En rechargeant la cible (`take_metadata_snap` puis `era_invalidate`), les 9 blocs sont retrouvés : le noyau récupère `current_writeset` depuis le superbloc depuis « dm era: Recover committed writeset after crash » (2021). C'est pourquoi un disque arrêté n'est lu hors ligne qu'après un arrêt scellé ; sinon, c'est le démarrage suivant qui récupère ses écritures.

**Pistes ouvertes.** Si les pauses liées à de gros tours posent problème, lire les blocs modifiés après le dégel, depuis un instantané `dm-snapshot` temporaire placé sous dm-era.
