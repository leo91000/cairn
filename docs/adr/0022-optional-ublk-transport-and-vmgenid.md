# Transport ublk optionnel et VMGenID

Date : 2026-10-02. Statut : gain mesuré en production ; activation ublk retirée
pendant la correction du blocage de writeback. Les snapshots restent distincts.

## Question et mesures

Remplacer vhost-user par ublk et virtio-blk Async réduit-il le coût disque
sans perdre le regroupement durable ? Le prototype utilise le journal LazyDisk de
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

## Transport géré et qualification du runner

Le runner peut maintenant sélectionner `LEO_BLOCK_TRANSPORT=ublk` ; vhost-user
reste le choix par défaut. Le moteur Async utilise le même `Volume`, journal,
regroupement des écritures durables et frontière de publication. La construction
Docker et la CI compilent cette option et ses tests ; l'activation sur les nœuds
et celle des snapshots restent séparées.

Le noyau choisit les numéros de périphériques. Une intention fsyncée précède ADD,
puis un reçu associe le numéro à un nonce immuable du noyau, au boot, à la VM et
à l'identité du processus backend. Cela couvre un crash avant l'enregistrement
du numéro. Un ancien numéro réutilisé ne permet pas de retirer le nouvel
occupant. Le nettoyage attend la mort du backend et de la VM ; STOP précède
DEL, avec confirmation. Une fermeture normale utilise les processus déjà
attendus, sans inspection des jails voisins. Le réveil eventfd est acquitté
après le dernier slot d'I/O, avant de quitter le runtime de libublk.

La qualification locale utilise le binaire Debian candidat dans l'image guest
de v0.52.3, avec les capacités limitées du runner et les permissions des
périphériques ublk ajoutées. Les tours, reprises, captures pendant les nouvelles
écritures, crash/recovery, redimensionnement, renouvellement de l'entrypoint,
sandboxes et annulation/remplissage du pool passent. La charge de 304 secondes
effectue 27 431 écritures durables et 46 publications : p99 final 25,7 ms,
maximum 239 ms, aucun redémarrage ni erreur de lecture/écriture. Les groupes
durables restent présents (maximum de 7 frames sur cette charge séquentielle).
La pause de capture médiane vaut 59 ms, au maximum 96 ms.

Après l'arrêt gracieux du contrôleur, aucun reçu ni périphérique du banc ne
reste. Les logs ne contiennent plus de refus de fermeture ni de slab libublk
abandonné. Ces deux défauts ont été observés sur une première exécution,
corrigés, puis le scénario complet a été répété. Le test des propriétaires
vérifie aussi le refus de nettoyage avec une VM encore vivante, la récupération
d'une intention sans numéro et l'intégrité du journal après SIGKILL.

L'origine de cette qualification est HTTP local, pas S3/WAN ; ce n'est pas une
mesure du premier appel modèle en production, ni une activation des snapshots.

## VMGenID

Le noyau livré désactive `VIRT_DRIVERS` et n'inclut pas VMGenID : un test privé
échoue sur l'absence du pilote attaché. Le candidat active les deux options en
built-in ; la construction vérifie `CONFIG_VMGENID=y` après `olddefconfig`.
Avec ce noyau 6.12.109, quatre restaurations partagent le même inode mémoire,
gardent Codex prêt et reçoivent chacune exactement une notification de fork
qui provoque le reseed du noyau, confirmé par dmesg. Les quatre échantillons
`getrandom` sont distincts. Entre la demande de chargement et l'observation du
reseed par vsock, les temps valent 18,94 / 19,26 / 20,21 / 12,06 ms ; ils ne
mesurent ni le seul interrupt ni le démarrage complet en production.
Le prébuild de ce banc prend 8,62 s et les VM sont supprimées à la fin.

Ce résultat ne qualifie pas les caches RNG de l'espace utilisateur ni une
barrière d'admission du premier tour. L'intégration doit encore renouveler
l'identité réseau et qualifier l'entropie avant le travail de la conversation ;
les threads distincts du banc ne prouvent pas à eux seuls ce point. Voir les
[recommandations de Firecracker 1.17](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/docs/snapshotting/random-for-clones.md).

Les résultats et scripts détaillés restent hors Git pour les assets de release.

## Qualification en production

La PR #34 est mergée en `894ed3f`. Les mesures utilisent exactement son image
immutable `sha256:38dd9dcf11bb22e53f7db944d05ab43d38991fafc279257d5f6f97428aa8aa93`,
sur le même VPS, dépôt et script attachés. L'ordre vhost/ublk/ublk/vhost donne
six répétitions par transport, avec les sauvegardes S3 périodiques activées.
Chaque valeur est la médiane des six médianes de répétition ; démarrage, import
et préparation sont exclus. Le transport est le seul paramètre modifié.

| Opération, ms | vhost-user | ublk + Async | Variation |
| --- | ---: | ---: | ---: |
| fsync | 10,31 | 7,47 | −27,6 % |
| fdatasync | 2,84 | 2,92 | +2,9 % |
| Petit fichier durable | 19,66 | 17,35 | −11,7 % |
| git status après installation | 12,19 | 13,57 | +11,3 % |
| npm ci hors ligne | 17 876 | 17 464 | −2,3 % |

Le gain de fsync est confirmé ; aucun gain npm ni de démarrage n'est établi.
Les quatre tours réussissent avec leur sauvegarde finale acquittée. Un cinquième
tour ublk, exclu de la comparaison, fournit les compteurs avant déchargement du
disque : 24 308 écritures, 3 644 lectures, aucune erreur, maximum de 63 frames par
groupe durable et zéro octet lu depuis S3. Sa sauvegarde finale est acquittée.
La préparation de capture dure encore 1,36 à 4,51 s sous charge, tandis que
seal/resume dure 4 à 7 ms ; un fsync atteint 4,54 s. Cette limite de latence
reste visible et n'est pas présentée comme résolue par le changement de transport.

Le scénario complet de runner passe avec cette même image et ublk : reprise,
redimensionnement, annulation, capture pendant les écritures, crash/recovery et
pause de sécurité lors d'une indisponibilité de stockage. Après son arrêt
gracieux, aucun reçu ni périphérique du banc ne reste. Sur le noyau du VPS, le
banc isolé vérifie aussi la réutilisation des numéros, le refus de retirer un
propriétaire vivant, la récupération des intentions après SIGKILL et l'intégrité
du journal. Ces tests ne qualifient pas un démarrage par snapshot en production.

## Périmètre de livraison

### Progression du writeback hôte

La qualification Android a révélé un worker bloqué dans `balance_dirty_pages`.
Le ménage disque effectué sur l'hôte entre 14 h 45 et 15 h 05 UTC augmente
la contention de cette première exécution ; il ne suffit pas à expliquer
le défaut. Après ce ménage, un banc sans VM, sans remplissage mémoire ni charge
externe ajoutée reproduit le blocage avec 512 Mio d'écritures tamponnées sur
un périphérique ublk réel, dans un conteneur limité à 1 280 Mio. Le témoin
expire après 20 s, sans OOM. Ajouter uniquement `CAP_SYS_RESOURCE` ne le corrige
pas : libublk marque sa file, mais le worker qui écrit le journal reste ordinaire.

Le backend sert le writeback d'un périphérique tout en écrivant via le page
cache d'un autre : le throttling peut attendre les requêtes qu'il doit lui-même
terminer. Le runner ublk active et vérifie `PR_SET_IO_FLUSHER` avant de créer
son runtime ; ses threads de journal, capture et maintenance héritent de cette
protection. Le worker du journal la vérifie aussi explicitement. Sans la
capacité, le démarrage échoue avant la création d'un périphérique. Seuls les
conteneurs configurés pour ublk reçoivent `CAP_SYS_RESOURCE` ; les plafonds
mémoire du conteneur et la frontière durable restent en place.

Le test noyau `tests/ublk-writeback-smoke.mjs IMAGE PROBE` couvre la progression
d'un Gio, le regroupement durable, l'absence d'OOM, un SIGKILL, la relecture
complète après réouverture du journal et le refus sans capacité. `PROBE` est
construit avec `--features ublk-prototype --example ublk_probe` pour la libc de
l'image testée. Les détails et pressions hôte sont conservés hors Git dans
`/var/tmp/leo-ublk-evidence-*`. L'ancien binaire échoue sur la progression après
20 s ; le candidat réussit trois répétitions consécutives sans charge ajoutée
en parallèle : écriture et fsync en 2,61 / 3,51 / 3,41 s, 2 052 frames durables,
groupes de 16 frames au maximum, aucun OOM et un Gio vérifié après chaque crash.
Ces temps portent sur le banc, pas sur une conversation. La qualification Android et l'image finale
restent des étapes requises avant release ; les mesures ci-dessus ne les remplacent pas.

Cette décision livre uniquement le transport optionnel et le pilote VMGenID.
La séparation système/workspace, les vues de disque, le prébuild et le démarrage
par snapshot sont conservés dans une branche distincte pour une prochaine PR.
Le correctif des artefacts rejetés est livré séparément dans la PR #35.
Le choix par défaut reste vhost-user. Une release du transport exige un gain
mesuré en production, comparé à vhost-user sur le même dépôt et la même charge.

Le superviseur accepte `blockTransport: "ublk"` dans sa configuration locale.
Avant de remplacer un conteneur, il charge `ublk_drv`, vérifie le contrôle et
io_uring, puis détecte les classes de périphériques dans `/proc/devices`.
Il expose seulement le contrôle ublk et autorise les mineurs dynamiques de
ces classes, sans mode privilégié ni accès global aux périphériques. Le major
bloc `blkext` peut être partagé avec d’autres pilotes : cette permission reste
réservée au backend de confiance, jamais exposée aux guests. La configuration
par défaut ne charge pas ce module et n’ajoute aucune permission ublk.

Pour le runner local géré par Coolify, le choix explicite
`LEO_BLOCK_TRANSPORT=ublk`, le montage `/dev/ublk-control` et les deux classes
détectées doivent figurer dans la configuration du service. Le déploiement
conserve ces paramètres et refuse une configuration ublk incomplète avant de
modifier ou redémarrer le service. Les valeurs sont propres à chaque hôte.
