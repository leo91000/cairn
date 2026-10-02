# Templates anonymes avec mémoire privée partagée

Date : 2026-10-02. Statut : prototype intégré et mesuré localement ; activation
production et release non qualifiées. Cette PR suit la livraison des sauvegardes
sans gel de v0.52.5 ; elle conserve le transport ublk de v0.52.4.

## Décision

Firecracker démarre avec ublk et virtio-blk Async. La mémoire du guest n'est plus
le memfd partagé qu'exige vhost-user. Les clones restaurent le même inode de
snapshot avec le backend `File`, mappé en privé : leurs modifications restent
indépendantes et les pages inchangées sont partagées par le noyau hôte.

Un disque de conversation devient deux vues bornées du même journal durable :
le système/overlay et le workspace. Les deux vues conservent la même frontière
de génération et le regroupement des écritures/fsync. Par défaut, le système
occupe un quart du budget disque. Les deux systèmes de fichiers sont agrandis
hors ligne, sans réduire la partition système existante ; une reprise après
crash repart du journal original plutôt que d'une image partiellement déplacée.
Les disques existants gardent leur format. Un manifeste à deux vues porte la
version 2 : un ancien lecteur le refuse au lieu d'ignorer le workspace.

Le template est construit sans compte, accès S3, projet, conversation ni
workspace monté. Codex est initialisé avec l'entrypoint courant. Le système est
gelé, la VM suspendue et sa génération scellée avant la capture. Le premier
snapshot `Diff` d'une VM démarrée avec suivi des pages sales est autonome selon
[Firecracker 1.17](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/docs/snapshotting/snapshot-support.md).
Ce choix évite de matérialiser toute la RAM maximale de la VM. Il n'y a ni chaîne
de diffs, ni snapshot d'une conversation. Ce chemin doit rester qualifié contre
la version Firecracker épinglée ; le support des diffs est annoncé expérimental
par l'amont.

Après restore, les deux drives sont remplacés pendant la pause par les vues du
journal propre au clone. Le disque système reproduit exactement la génération
capturée ; le workspace reste démonté jusqu'au remplacement. Le guest dégèle,
renouvelle son réseau, reçoit 256 bits d'entropie hôte crédités au noyau et un
identifiant de clone, puis recrée le processus natif Codex avant tout accès.
VMGenID seul ne garantit pas le renouvellement des générateurs conservés en
mémoire par une application, comme le précise
[l'amont](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/docs/snapshotting/random-for-clones.md).
L'horloge est resynchronisée. Le workspace est ensuite monté et la conversation
attribuée durablement ; ses accès sont renouvelés par le chemin normal.

Le cache est construit par nœud et configuration : runtime, binaire courant,
kernel guest, Firecracker, CPU/kernel hôte, vCPU, RAM, géométrie disque et politique
stockage. Deux configurations complètes au maximum sont conservées. Les clones
lient la mémoire/état immuables et copient le journal hors ligne, sans lier ses
fichiers modifiables. Une éviction ne supprime pas les inodes liés par des clones
vivants. Les répertoires de construction incomplets sont nettoyés après clôture
du contrôleur précédent. Le pool préconstruit le template ; une arrivée neuve
peut aussi restaurer un template déjà complet, sans attendre sa construction.
Les restaurations sont parallèles ; installation/éviction seulement utilisent
un verrou exclusif. Un échec de restauration retire le clone et ne lui attribue
aucune conversation. Les reprises existantes gardent la rétention/boot habituel.

## Premier comparatif intégré

Même image privée dérivée de v0.52.5 avec le nouveau runner/guest, Firecracker
1.17, Codex natif et ublk Async. Quatre conversations simultanées, enveloppe de
4 CPU/16 Gio, plafond guest de 15,5 Gio et disque de 32 Gio. Une VM est déjà prête
dans le pool ; les trois autres arrivées utilisent le boot ou le template.
Le modèle et le compte sont synthétiques, l'origine de publication est HTTP
locale : ce banc ne mesure pas S3/WAN ni l'UI en production. Le modèle retarde sa
réponse finale pour mesurer quatre VMM réellement actifs après leur premier appel.

| Mesure | Pool + boot normal | Pool + snapshots |
| --- | ---: | ---: |
| RAM du cgroup complet, template/cache inclus | 4,70 Gio | 3,41 Gio |
| PSS cumulé des quatre VMM | 3,53 Gio | 1,29 Gio |
| Mémoire privée sale des VMM | 3,53 Gio | 0,99 Gio |
| Avant modèle, arrivée qui trouve le pool | 2,66 s | 2,80 s |
| Avant modèle, trois autres arrivées | 7,73–8,61 s | 3,04–3,28 s |

Le cgroup baisse de 27,4 % et le PSS de 63,6 %. Les quatre mappings `rw-p` pointent
le même inode ; leurs journaux restent distincts. Un template de 15,5 Gio
virtuels occupe environ 723 Mio sur disque. Aucun OOM n'est observé. Il s'agit
du premier comparatif : la pression I/O de l'hôte est élevée et le premier hit
du pool ne s'améliore pas. Ne pas extrapoler ces latences à la production ni annoncer
un gain sur les reprises. La copie durable du journal prend 1,34–1,41 s sous la
rafale, davantage que le restore/montage/renouvellement de 0,44–0,54 s.

Deux répétitions complètes confirment le partage sur douze conversations par
mode : 4,70–4,87 Gio contre 3,41–3,46 Gio pour le cgroup, soit 27,4–28,9 % de
moins ; le PSS des VMM baisse de 63,4–63,9 %. Aucun OOM. Toutes les VM restaurées
partagent l'inode propre à leur cohorte, et leurs identifiants restent distincts.
La pression I/O est relevée avant/après chaque répétition ; elle limite encore
les conclusions sur les latences et doit être distinguée du partage mémoire.

Trois cycles supplémentaires publient le disque adopté, tuent le contrôleur par
SIGKILL, restaurent le point publié et reprennent le même thread natif. Les
blocs sont vérifiés ; commande outil, thread et réponse finale sont contrôlés.
Cela qualifie la mécanique de reprise, pas encore une charge prolongée.

Les mesures brutes et scripts de benchmark restent hors Git pour les assets de release.
Le test natif reproductible est versionné :
`node tests/runner-snapshots-smoke.mjs IMAGE`, puis la même commande avec
`LEO_VM_SNAPSHOTS=false` pour le contrôle. Il requiert KVM et `ublk_drv`, conserve
le modèle synthétique hors du guest pour vérifier le réseau renouvelé, contrôle
quatre mappings privés vers le même inode et vérifie après SIGKILL des marqueurs
fsync dans le système et le workspace de chaque conversation. Les seuls accès
de test sont synthétiques. `SYS_PTRACE` sert à la mesure smaps, pas au produit.

Le test versionné passe ensuite sur une image sans modèle ajouté au guest,
toujours dérivée du runtime épinglé de v0.52.5 et avec le nouveau runner/guest.
Quatre conversations par mode publient puis reprennent après SIGKILL ; les deux
marqueurs et le même thread natif survivent dans les huit cas. RAM totale :
4,95 Gio sans snapshots contre 3,70 Gio avec snapshots (−25,3 %) ; PSS cumulé :
3,66 Gio contre 1,36 Gio (−62,7 %). Avant modèle : 2,59 s pour le hit ordinaire du
pool et 7,92–7,94 s pour les autres arrivées, contre 3,63–3,71 s pour les clones.
Le hit du pool n'est donc toujours pas une victoire de latence. La pression I/O
hôte reste élevée ; ces mesures ne remplacent pas la qualification production.

Le contrôle sans rafale donne aussi 1,65–2,91 s avec le pool ordinaire et
1,39–1,93 s avec snapshots sur trois conversations par mode. L'échantillon est
trop petit et la charge hôte trop variable pour annoncer un gain de latence isolé.

## Charge sur les deux vues

Quatre VM par mode écrivent, fsync et relisent en continu des blocs de 4 Kio
dans le système **et** le workspace. Chaque mode subit cinq minutes de captures
crash-consistent et publications HTTP locales, puis une publication finale et
SIGKILL du contrôleur. Les huit reprises retrouvent leur thread, leurs marqueurs
et des blocs complets dont numéro de séquence, emplacement et contenu sont vérifiés.
Les 332 captures périodiques gardent `pauseMs=0` ; aucune attente de stockage,
erreur de lecture/écriture ou substitution de VM n'est observée.

| Fsync de 4 Kio | Boot ordinaire, même journal à deux vues | Snapshots |
| --- | ---: | ---: |
| Échantillons collectés | 58 838 | 60 566 |
| p99 global / maximum | 53,1 / 928,2 ms | 52,8 / 852,2 ms |
| p99 / maximum pendant capture | 270,0 / 646,1 ms | 153,8 / 471,8 ms |
| p99 / maximum pendant publication | 150,3 / 928,2 ms | 73,8 / 852,2 ms |

Le p99 global reste similaire. Les fenêtres sont rapprochées par les horloges
hôte/guest synchronisées, à la résolution de la milliseconde. La pression I/O
hôte est élevée et varie entre modes : les pics plus faibles ne constituent pas
une preuve de gain causal sur les écritures. Ce comparatif contrôle le restore
contre le boot ordinaire, avec le même transport ublk/journal ; ce n'est ni un
comparatif avec disque direct, ni une mesure S3 de production.

Admission/annulation sous pression, image finale Intel/AMD et mesure en production restent
nécessaires avant livraison.

## Activation et compatibilité

Opt-in : `LEO_VM_SNAPSHOTS=true`, `LEO_BLOCK_TRANSPORT=ublk` et
`LEO_DISK_LAYOUT=paired-ext4-v1`, avec le binaire natif et le nouveau guest.
Le pool est activé pour préconstruire la configuration au démarrage du contrôleur.
Sans option snapshots, le comportement actuel reste le défaut. Les petits
disques de moins de 256 Mio restent plats ; le chemin préparé utilise 32 Gio.

Avant de créer des disques version 2 en production, le manager et tous les nœuds
autorisés à les reprendre doivent lire ce format et utiliser ublk. Un nœud
vhost-user refuse ces disques. Désactiver les snapshots garde les disques à deux
vues utilisables avec ublk ; revenir au transport vhost-user nécessite d'abord
une migration explicite de ces disques. Aucune suppression de conversation
existante n'est requise.
