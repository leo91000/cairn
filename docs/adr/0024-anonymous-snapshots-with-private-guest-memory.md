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
occupe un quart du budget disque. Docker/containerd, les installations du toolkit
(`~/.local/share`), les caches (`~/.cache`) et les données Android (`~/.android`)
sont bind-montés depuis le workspace ; `~/.codex` reste sur la vue système.
Une copie existante est installée et synchronisée atomiquement avant suppression
de l’ancienne copie, puis les copies incomplètes d’un crash sont nettoyées.
Le premier test Android a révélé qu’un userdata de 12 Gio ne tenait pas dans
les 8 Gio système du disque par défaut ; augmenter seulement le budget total
n’aurait pas corrigé ce placement. Les deux systèmes de fichiers sont agrandis
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

Le template démarre avec `kvm.enable_virt_at_load=0`. Le noyau guest 6.12 active
normalement VMX/SVM dès son chargement ; cet état matériel de virtualisation
imbriquée n'est pas restauré avec le template. Le premier prototype fait ainsi
échouer les quatre créations de vCPU L2 sur Intel (`kvm_spurious_fault` et
SIGSEGV), alors que le contrôle à froid passe. Avec l'activation différée au
premier L2, les quatre clones et leurs quatre reprises après crash exécutent
`KVM_RUN` et calculent 42. Le guest refuse la capacité de template si le réglage
n'est pas effectif. Les démarrages ordinaires gardent leur configuration.
Ce réglage est celui du
[noyau](https://docs.kernel.org/admin-guide/kernel-parameters.html) ; aucun patch
Firecracker n'est ajouté. Le template reste sans VM Android/nested active.

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
Le même test compile et exécute le petit guest L2 de `nested-kvm.c` avant chaque
marqueur, dans les clones puis après reprise ; il ne remplace pas la qualification
Android complète sur les deux architectures.

Avant l’ajout du contrôle L2, le test natif passe sur une image sans modèle ajouté
au guest, toujours dérivée du runtime épinglé de v0.52.5 et avec le nouveau runner/guest.
Quatre conversations par mode publient puis reprennent après SIGKILL ; les deux
marqueurs et le même thread natif survivent dans les huit cas. RAM totale :
4,95 Gio sans snapshots contre 3,70 Gio avec snapshots (−25,3 %) ; PSS cumulé :
3,66 Gio contre 1,36 Gio (−62,7 %). Avant modèle : 2,59 s pour le hit ordinaire du
pool et 7,92–7,94 s pour les autres arrivées, contre 3,63–3,71 s pour les clones.
Le hit du pool n'est donc toujours pas une victoire de latence. La pression I/O
hôte reste élevée ; ces mesures ne remplacent pas la qualification production.

Après le correctif d’activation KVM différée, le même test compile et lance le
L2 dans les quatre clones puis leurs quatre reprises après crash. Tous les
`KVM_RUN` calculent 42. Avec ce travail supplémentaire, le cgroup complet passe
de 5,36 Gio à 4,28 Gio (−20,1 %) et le PSS cumulé de 3,82 Gio à 1,59 Gio (−58,4 %).
Ce sont des cohortes distinctes de la mesure sans compilation ci-dessus ; le
cache du compilateur et du noyau fait partie du coût réel mesuré. Le partage
reste établi après le lancement de processus utilisateurs, sans promettre une
réduction de RAM constante pour toutes les charges.

Le contrôle sans rafale donne aussi 1,65–2,91 s avec le pool ordinaire et
1,39–1,93 s avec snapshots sur trois conversations par mode. L'échantillon est
trop petit et la charge hôte trop variable pour annoncer un gain de latence isolé.

## Image immutable sur les deux nœuds Intel

La CI de `a1d89e2` est verte. L’image `112f0180…29c05` passe le test natif à
quatre VM sur i9-14900K et sur le VPS Xeon E5-1410 v2. Chaque nœud utilise la
même image pour le contrôle sans snapshots, toujours avec ublk et deux vues.

| Quatre VM avec compilation/exécution L2 | PC Intel | VPS Intel |
| --- | ---: | ---: |
| Cgroup sans snapshots / avec snapshots | 5,24 / 4,03 Gio | 5,26 / 4,21 Gio |
| Réduction du cgroup complet | 23,1 % | 19,9 % |
| PSS sans snapshots / avec snapshots | 3,80 / 1,58 Gio | 3,84 / 1,75 Gio |
| Reprises après publication et SIGKILL, deux modes | 8 / 8 | 8 / 8 |

Les comptes/modèles et l’origine HTTP sont synthétiques. Ce sont des mesures
sur le matériel des nœuds, sans activation sur les conversations de production.
La pression I/O du VPS est élevée ; les arrivées en rafale avant modèle donnent
8,20–11,69 s avec snapshots contre 13,64–24,93 s sans. Ce banc change aussi la
configuration du modèle, ne mesure pas UI/S3/WAN et n’est pas comparable au hit
habituel du pool ou à une reprise retenue. Aucun OOM ; les mappings privés des
quatre clones partagent un inode. La réduction du budget pendant la capture
annule la VM source en 269 ms, n’installe aucun template incomplet, puis la
préparation suivante réussit sur cette image.

Ces chiffres précèdent le correctif de placement des caches Android décrit
ci-dessus : son nouveau guest reste à qualifier sur l’image finale. AMD et le
chemin production complet ne sont pas encore qualifiés.

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

Admission/annulation sous pression et mesure en production restent nécessaires
avant livraison. Les deux nœuds de production sont Intel ; aucune qualification
matérielle AMD n'est revendiquée.

## Activation et compatibilité

Opt-in : `LEO_VM_SNAPSHOTS=true`, `LEO_BLOCK_TRANSPORT=ublk` et
`LEO_DISK_LAYOUT=paired-ext4-v1`, avec le binaire natif et le nouveau guest.
Le pool est activé pour préconstruire la configuration au démarrage du contrôleur.
Sans option snapshots, le comportement actuel reste le défaut. Les petits
disques de moins de 256 Mio restent plats ; le chemin préparé utilise 32 Gio.

Le superviseur installé sur l'hôte conserve ces options dans son `config.json` :

```json
{
  "blockTransport": "ublk",
  "diskLayout": "paired-ext4-v1",
  "vmSnapshots": true
}
```

Pour Coolify, renseigner les trois variables ci-dessus dans l'environnement du
runner ; les valeurs `"true"` et `"false"` doivent être des chaînes YAML. Le
générateur Compose et le superviseur conservent cette configuration lors des
mises à jour. Une combinaison incompatible est refusée avant de modifier le
service ou d'arrêter le nœud. Pour désactiver les nouveaux clones, passer seulement
`vmSnapshots` / `LEO_VM_SNAPSHOTS` à `false` et conserver le transport et le format.

Avant de créer des disques version 2 en production, le manager et tous les nœuds
autorisés à les reprendre doivent lire ce format et utiliser ublk. Un nœud
vhost-user refuse ces disques. Désactiver les snapshots garde les disques à deux
vues utilisables avec ublk ; revenir au transport vhost-user nécessite d'abord
une migration explicite de ces disques. Aucune suppression de conversation
existante n'est requise.
