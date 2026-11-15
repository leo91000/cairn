# Répertoires natifs et images VM locales sans transport applicatif

Décision demandée par Léo le 14 novembre 2026. Cette décision remplace la partie
format/transport de l’[ADR-0083](0083-local-conversation-disks-without-s3.md), les
journaux et les bases préparées de l’[ADR-0052](0052-native-ublk-workspaces-and-idle-eviction.md),
ainsi que l’obligation ublk de l’[ADR-0055](0055-ublk-only-vm-storage.md).
L’environnement reste local à sa node. L’expiration configurable, les confirmations
de changement de node et de révocation, la corbeille, l’historique et les artefacts
conservent leurs contrats. La suppression de S3 ne désactive pas la rétention.

Une exécution native utilise un répertoire privé retenu pour ses fichiers, son
home, ses données Docker et son temporaire géré. Le contrôleur conserve ses
métadonnées de contrôle sur un système de fichiers distinct du pool de données : un environnement plein
doit encore pouvoir reprendre, appliquer une capacité accordée et libérer ses
fichiers. La première implémentation vise un système de fichiers ext4 dédié, avec
quotas de projet effectivement appliqués, monté sous `state/native-data`.
Les répertoires de conversation y sont ordinaires ; aucun montage ni périphérique
n’est créé pour un nouvel environnement. Le compteur de projets, les reçus et les
intentions de nettoyage restent sur le système de fichiers de contrôle. Les
limites d’inodes utilisent la géométrie réelle du pool, et son épuisement ne peut
pas empêcher le contrôleur de consigner une expiration. Les limites de blocs et d’inodes sont
imposées par le noyau ; les cgroups ne bornent pas l’espace occupé. La comptabilité
partagée utilise les octets alloués et les crédits d’appartenance vérifiés, sans
facturer les trous d’un fichier sparse comme des blocs physiques.

La VM reçoit directement un fichier ext4 local sur son interface bloc Firecracker.
Sa taille borne sa capacité. Pendant que le VMM peut écrire, une réservation
locale facture toute cette capacité au budget partagé, même si le fichier reste
sparse ou si le guest effectue un discard. Le budget distingue ces promesses des
octets physiquement alloués. Une époque d’admission fige l’enveloppe physique hors
écritures des VM ; une surveillance séparée suspend ou arrête les écritures si la
réserve physique devient indisponible. L’admission peut attendre malgré une faible
occupation des images. Avant de refuser une promesse d’écriture demandée par une
exécution, elle récupère les promesses des VM inactives éligibles, y compris la VM
anonyme préparée et les conversations retenues. Chaque VMM est arrêté et son
image synchronisée avant cette libération ; les images de conversation restent
retenues. Les promesses des exécutions actives ne sont jamais reprises. Après arrêt confirmé et flush terminé, elle libère la
promesse inutilisée ; l’image retenue est alors facturée à son allocation réelle. L’image est retenue après arrêt, et la même node la
rouvre au tour suivant. Aucun journal applicatif, transport ublk, copie distante ou
consolidation périodique ne s’interpose entre ces nouveaux environnements et leur
système de fichiers. Les caches de projets natifs sont des arbres de fichiers
vérifiés ; leur copie privée n’emprunte pas l’autorité d’une autre conversation.

Les limites CPU, RAM et processus restent des cgroups systemd. Le contrôleur exige
aussi le contrôleur IO et applique une limite de lecture et d’écriture de 64 MiB/s
par exécution sur le système de fichiers du travail, sous un plafond partagé de
256 MiB/s. Ces plafonds sont distincts de la priorité relative IO et du quota de
stockage. Leur qualification doit vérifier les valeurs réellement appliquées.

Arrêter une exécution ferme ses processus, ses conteneurs attribués et ses
ressources temporaires, tout en retenant les fichiers nécessaires à une reprise.
Le cgroup principal vide ne suffit pas à prouver l’arrêt de consommateurs gérés
par un autre service, notamment le Docker administrateur. L’appartenance doit
être conservée et revérifiée avant toute action ; un PID ou numéro de périphérique
historique ne constitue jamais cette preuve. Une clôture incertaine conserve
l’ownership et une erreur observable, avec reprise du nettoyage.

L’expiration ou la suppression de l’environnement efface ensuite ses répertoires,
images, volumes et données Docker privées ainsi que les restes techniques
attribués. La file durable et ses reçus de retraite restent idempotents après crash,
redémarrage ou retour d’une node hors ligne. Un reçu d’un ancien environnement ne
peut pas effacer le suivant. Les petits reçus d’idempotence et les inodes de verrou
restent volontairement conservés : les retirer rendrait possibles une répétition
destructrice ou deux propriétaires concurrents. Les caches partagés de projets
restent soumis à leur budget propre ; ils n’appartiennent pas au travail privé
d’une conversation.

Les journaux locaux existants ne sont pas déclarés vides. Leur conversion garde
la source jusqu’à la vérification complète et à la publication durable du format
remplaçant. Pour une VM, les octets exportés sont comparés à la source avant
vérification ext4 et bascule vers l’image. Tant que la source peut subsister,
le marqueur de conversion lie le système de fichiers, l’inode et le digest complet
de l’image vérifiée. Une reprise valide cette preuve avant tout fsck, croissance
ou effacement de source. L’effacement devient durable avant le retrait de la
preuve et avant de confier l’image mutable au VMM. Une preuve absente dans un
état mixte ou un remplacement modifié est refusé en conservant la source ; ces
formats de développement ne sont pas réparés par une perte silencieuse.
Pour le natif, le chemin de compatibilité
sert seulement à copier et vérifier les fichiers retenus avant de retirer l’ancien
transport. Toute phase interrompue doit être rejouable ; les anciens fichiers ne
sont effacés qu’après la bascule durable. Un format inconnu ou une source manquante
est refusé explicitement, sans initialiser silencieusement un environnement vide.
Le retour à l’ancien format exige une sauvegarde ; aucun déploiement de cette
migration en production n’est autorisé par cette implémentation.

Le merge exige les qualifications natives et VM sur infrastructure jetable,
plusieurs cycles création/écriture/arrêt/reprise/expiration, les cas quota et
ENOSPC, arrêt forcé, crash, consommateurs Docker survivants et nettoyage retardé.
Les mesures comparent une référence exacte aux nouveaux chemins : octets logiques
et alloués, lectures froides/chaudes, écritures et CPU, avec commandes et conditions
de l’hôte. Elles restent locales, sans inférence sur une latence réseau de production.

Ext4 project hard limits apply to processes without `CAP_SYS_RESOURCE`.
[Linux quota enforcement](https://raw.githubusercontent.com/torvalds/linux/v6.12/fs/quota/dquot.c)
allows that capability to bypass hard limits, including project quotas. Protected
workloads and ordinary containers lack it. Native administrator authority remains
unrestricted: quota accounting and the controller's pressure/stop observations
remain useful, but an administrator can bypass or alter those limits. Migration
drops that capability from the copy child and checks allocated quota usage before
committing the copy, retaining
the legacy source when the copy exceeds its allowance. Disposable quota tests drop
that capability explicitly; they do not claim adversarial root confinement.

Les bases natives préparées deviennent des modèles de répertoires vérifiés,
remplaçant le clonage par préfixe de journal de l’[ADR-0058](0058-prepared-native-project-bases.md).
Chaque environnement reçoit des fichiers privés ; la copie et sa vérification
créent des inodes et ont un coût à mesurer face à l’extraction ZIP. Un lien de
contrôle nommant la clé du cache et l’exécution sert à la fois d’intention durable
et de pin du cache. Il survit au nettoyage des pins temporaires d’exécution.
Une copie interrompue reprend son arbre partiel privé ; un renommage terminé
est vérifié sans écrasement. Après publication durable du format, le nettoyage
d’une intention résiduelle conserve les modifications utilisateur ultérieures.
Les temporaires et les données Docker éphémères sont aussi retirés lors de la
récupération d’une exécution interrompue, avant son acquittement.

Après conversion native, l’ancien temporaire géré `state/native/<run>/tmp`
ne doit pas rester hors du nouveau quota. Sa retraite vérifie le sous-arbre exact,
son identité et ses consommateurs après l’arrêt de l’exécution. Un consommateur
inconnu conserve les fichiers et une erreur récupérable ; cette vérification ne
signale aucun processus. L’effacement et le fsync du parent précèdent la bascule
de migration, et la récupération des formats déjà publiés rejoue ce nettoyage.

L’admission d’une image VM attend uniquement la contention réelle du verrou de
capacité, avec annulation et délai de 30 secondes ; un conflit d’appartenance
reste un refus distinct. La migration native facture simultanément sa source
retenue et la copie supplémentaire au budget partagé. Elle exige aussi la réserve
physique du pool de données, observe le coût restant de la copie sous quota, et
interrompt son groupe rsync si ces conditions cessent d’être remplies. La source
reste retenue, le groupe est signalé avant de réutiliser son identifiant, et ses
consommateurs sont attendus avant la suppression de la copie partielle.

Lors d’un redimensionnement historique interrompu, l’ancien `data.ext4` reste
facturé pendant l’export du nouveau `image.partial`. Sa présence ne réduit pas la
promesse de capacité de remplacement. Le reçu de réservation distingue cet inode
historique de l’inode exporté : le crédit du nouveau fichier suit son renommage,
sans double promesse ni crédit des octets qui doivent encore coexister.
