# ublk et mémoire partagée des snapshots

Date : 2026-10-02. Statut : prototype local, non activé en production.

## Question et mesures

Remplacer le transport vhost-user par ublk et virtio-blk Async permet-il de
conserver le regroupement durable et de restaurer des VM avec une mémoire
commune en copie sur écriture ? Le prototype utilise le journal LazyDisk de
`cf7cb87`, Firecracker 1.17 et Codex 0.160.0. Aucun correctif de snapshot
vhost-user n'est ajouté. `ublk_drv` est chargé sur les deux nœuds ; leurs noyaux
sont Linux 7.2.6 et 6.8.0, avec io_uring autorisé.

Comparaison locale direct/vhost/ublk/ublk/vhost/direct : deux VM neuves par
transport, trois répétitions par VM, même dépôt Leo de 846 fichiers suivis et
650 paquets du lock npm. Chaque colonne donne la médiane des six médianes de
répétition. Installation hors ligne avec dépendances de développement, sans
scripts de cycle de vie ; démarrage, import et préparation exclus.

| Opération, ms | Disque direct | vhost-user | ublk + Async |
| --- | ---: | ---: | ---: |
| fsync | 7,35 | 10,64 | 7,40 |
| fdatasync | 3,55 | 3,60 | 3,62 |
| Petit fichier durable | 14,47 | 18,07 | 15,23 |
| git status après installation | 2,01 | 2,04 | 1,98 |
| npm ci | 2 740 | 2 765 | 2 822 |

Les groupes durables restent présents : maximum de 63 frames avec ublk et
85 avec vhost, aucune erreur de lecture/écriture. Les profondeurs de file
diffèrent : 64 pour ce prototype ublk, 128 pour vhost. Le gain local de fsync
est de 30,5 %, celui des petits fichiers de 15,7 % ; npm ne s'améliore pas.

## Restauration et mémoire

Une VM anonyme avec Codex prêt utilise un disque de démarrage natif Async.
Un second disque, jamais monté dans le template, reçoit après restauration
le disque ublk propre à chaque conversation via `PATCH /drives/conversation`.
Le guest monte ensuite ce disque. Les quatre restaurations partagent le même
inode mémoire en `rw-p` ; leurs disques de démarrage sont des copies distinctes.

La capture durable prend 9,50 s, à effectuer au prébuild. Le chargement de
chaque snapshot prend 3,8–5,0 ms et son PATCH disque moins de 1 ms. Après reprise,
le contrôle guest/Codex répond en 33–152 ms. Ces intervalles excluent la
préparation des disques/backends, le manager, les comptes et le premier modèle.

Quatre restaurations puis quatre VM indépendantes exécutent chacune un premier
tour et une reprise : seize tours avec le vrai Codex, un compte synthétique et
un modèle HTTP local. Les huit reprises gardent leur thread et leur contexte,
sans mélange entre VM ; les fichiers durables sont isolés. Après activité, la
somme PSS vaut **706 Mio contre 2 766 Mio**, soit **−74,5 %**. La mémoire du
snapshot reste inchangée. Une allocation privée de test de 128 Mio, lue dans
chaque VM, fait partie des deux mesures. Aucun swap du VMM n'est observé.

La PSS compte les pages mappées partagées une fois ; elle ne représente pas
tout le cgroup ni le cache non mappé des images. Les tours prennent environ
26 s dans les deux variantes de ce banc isolé : ils valident le fonctionnement
et la RAM après activité, pas la latence du premier modèle en production.

Un second banc utilise les deux vues ublk d'un journal unique pour chacune des
quatre VM, restaurées puis à froid : seize tours natifs, huit reprises de thread.
Après activité, le cgroup parent compte **1 212 Mio contre 3 201 Mio** (−62,1 %),
dont les caches facturés, les backends, journaux et contrôleur. Ses bases avant
chaque groupe valent 215 et 214 Mio ; leurs deltas baissent de 66,6 %. La PSS
des VMM seuls vaut 840 contre 2 792 Mio. Il s'agit d'une expérience séquentielle,
sans percentile ni qualification sous charge prolongée.

Le VMM du template est arrêté après prébuild et le contrôleur suspendu depuis
l'hôte, état vérifié. Seules les pages non mappées des fichiers du banc sont
conseillées DONTNEED avant les deux groupes ; aucun `drop_caches` global.
Le groupe froid utilise un OS neuf et la vraie initialisation anonyme, le groupe
restauré les octets de l'OS capturé. Les premiers essais avec HOME non vide,
puis avec un contrôleur encore actif, sont exclus de cette comparaison. Ce
cgroup mesure les pages facturées au banc, pas toute la mémoire physique hôte.

## Persistance du système et du workspace

Les descripteurs relevés dans Codex pointent déjà vers les bases SQLite et WAL
du disque de démarrage anonyme. Monter un autre HOME après restauration ne
déplace pas ces descripteurs. Ce prototype conserve donc ce disque ; il ne
ne démontrait pas encore la sauvegarde du système.

Une seconde expérience expose deux vues bornées d'un seul journal LazyDisk :
4 Gio pour le système/Codex, 4 Gio pour le workspace. La VM restaurée retrouve
les mêmes octets anonymes sous ses descripteurs ouverts ; le guest monte le
workspace ensuite. Un gel des deux filesystems puis une pause de la VM délimitent
une seule génération. Les écritures suivantes utilisent le même journal pendant
la capture et survivent à l'acquittement de l'ancienne génération.

Quatre tours natifs valident le même thread : deux tours initiaux, une reprise
à froid après SIGKILL du backend et du VMM, puis une reconstruction sans ancien
journal depuis la première génération publiée. Les modifications de `/etc`,
un exécutable installé dans `/usr/local/bin`, le workspace et le contexte Codex
survivent au crash. La reconstruction de la génération publiée retrouve le
premier contexte et exclut le deuxième, conformément à sa frontière. Aucune
erreur d'I/O ; le regroupement durable demeure. L'origine durable locale de ce
test remplace S3 : elle ne qualifie pas les grants ni le chemin réseau.

L'essai de crash a aussi révélé que STOP ne supprime pas un périphérique ublk
dont le backend a été tué. Le prototype supprime explicitement l'enregistrement
après confirmation de la mort du backend et du VMM, et vérification de l'état
DEAD. Le test complet repassé recrée les deux périphériques et les nettoie.

Le même test complet passe dans un banc isolé sur le VPS Linux 6.8.0 : quatre
tours natifs, même thread, reprise après SIGKILL et reconstruction indépendante
de la génération publiée. Les deux périphériques sont recréés puis supprimés,
sans erreur d'I/O. Le conteneur du banc est limité à 8 Gio et deux CPU. Cela
qualifie le transport sur ce noyau, pas le chemin manager/S3 de production.

## Intégration restant à qualifier

Un template anonyme sert aux nouvelles conversations ; une conversation ayant
déjà modifié son OS doit retrouver son disque propre à froid ou par rétention.
Les tailles fixes de ce prototype ne définissent pas le format de production :
le redimensionnement doit connaître les bornes de chaque filesystem. Mesurer le
coût mémoire complet du nœud, qualifier les leases/admission, le prébuild par
version/configuration et le premier modèle en production avant activation.

La configuration effective du noyau livré désactive `VIRT_DRIVERS` et n'inclut
pas VMGenID. L'intégration devra activer et vérifier ce pilote, ainsi que le
renouvellement de l'identité réseau et de l'entropie avant le premier tour. Les
threads distincts du banc ne prouvent pas à eux seuls ce dernier point ; voir
les [recommandations de Firecracker 1.17](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/docs/snapshotting/random-for-clones.md).

Les résultats et scripts détaillés restent hors Git pour les futurs assets
de release. Aucune activation ni release n'est justifiée par ce prototype seul.
