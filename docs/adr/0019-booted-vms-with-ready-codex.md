# VM pré-démarrées avec Codex prêt

Date : 2026-10-01.

Statut : prototypes mesurés ; admission du pool implémentée et qualification en cours.
Cette décision n'active pas encore de pool en production.

## Question et méthode

Le démarrage réel de v0.50.6 prend encore 8,75–10,05 s avant le modèle pour deux
conversations neuves sans projet. Avant toute nouvelle optimisation des blocs,
comparer un pool de VM déjà bootées et un snapshot Firecracker contenant un
Codex déjà initialisé, sans relancer Codex à chaque mesure.

Les prototypes utilisent l'image immutable v0.50.6 et son Firecracker 1.17.0,
le vrai Codex 0.159.3 et un modèle HTTP local simulé. Chaque VM du pool démarre
anonymement sur son propre disque ; son premier thread est créé à l'attribution.
Les reprises suivantes restent dans cette conversation. Les snapshots anonymes
créent chacun un thread distinct ; les snapshots d'une conversation existante
doivent retrouver sa réponse précédente. Le PID natif reste identique dans
l'invité après attribution, pause et restauration.

Les temps observés incluent l'import de la requête, les appels de contrôle,
la synchronisation de l'horloge après restauration et le polling. Ils vont
jusqu'au tour synthétique terminé : ils bornent donc le départ vers ce modèle.
Ils excluent l'authentification réelle, les MCP métier, S3, les projets et la
file du manager. Ils ne constituent pas un nouveau délai de production.

## Résultats

| Chemin | Temps observé | Ressources réelles et périmètre |
| --- | ---: | --- |
| Production v0.50.6, deux conversations neuves | 8,75–10,05 s | Message → départ modèle, 7 CPU / 35 Gio |
| Même banc vhost, démarrage complet puis premier tour, n=3 | 2,36–4,41 s | 7 CPU / 35 Gio ; pas d'authentification |
| Pool vhost avec Codex prêt, attribution initiale, n=3 | 0,25–0,48 s | Même banc et mêmes ressources |
| Codex conservé dans la même VM/conversation, n=6 | 0,14–0,19 s | Même banc ; contexte retrouvé |
| Pause puis reprise, horloge et tour, n=1 | 0,33 s | Même Codex et même contexte |
| Snapshot anonyme, cache mémoire chaud, n=2 | 0,59–0,70 s | Restauration + premier tour ; 3 CPU / 5,5 Gio |
| Snapshot anonyme, cache mémoire évincé, n=1 | 1,36 s | Zéro page du fichier mémoire en cache, vérifié par mincore |
| Snapshot avec contexte, cache chaud, n=2 | 0,31–0,38 s | Restauration + reprise du contexte |
| Snapshot avec contexte, cache évincé, n=1 | 1,10 s | Même contrôle de cache |

La configuration effective est lue dans Firecracker : les limites partagées
remplacent les ressources demandées dans le plan. Le banc snapshot utilise
donc réellement 5,5 Gio, et non les 4 Gio demandés initialement.

La création d'un snapshot complet prend 12,59–14,19 s dans la série avec
éviction, contre 7,69–11,50 s dans la première série. Chaque fichier mémoire
occupe 5,5 Gio. La restauration du VMM seule prend 23–48 ms dans la série avec
éviction ; cela ne comprend pas les défauts de pages et le travail de Codex.
Le VMM vhost conservé utilise environ 1,08 Gio de RSS dans le banc à 35 Gio.
La pause ne libère pas cette mémoire. Le compteur du cgroup inclut également
le contrôleur et le cache des images ; ce n'est pas la mémoire d'une VM seule.

## Décision et limites d'intégration

Retenir le pool comme prochaine implémentation, en conservant le journal
durable et le transport vhost-user. Sur notre image actuelle, l'appel réel
`PUT /snapshot/create` renvoie HTTP 400 : le périphérique vhost-user-block ne
supporte pas les snapshots. Le banc snapshot remplace explicitement ce disque
par un fichier ext4 virtio classique privé ; il contourne ainsi notre journal.
Il prouve le potentiel de restauration, mais pas la compatibilité avec le
stockage de production. Le [code Firecracker épinglé](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/src/vmm/src/device_manager/persist.rs)
et la [documentation de snapshot](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/docs/snapshotting/snapshot-support.md)
décrivent les contraintes de périphériques, disques et connexions vsock.

L'intégration doit respecter les frontières suivantes :

- Une VM anonyme du pool est attribuée une seule fois ; aucun compte ni workspace
  utilisateur n'entre dans un template partagé. L'authentification commence après
  la réservation du compte et reste liée à l'exécution autorisée.
- Une VM conservée après un tour appartient exclusivement à sa conversation et
  à une configuration compatible. Les changements de compte, droits, sandbox,
  runtime ou configuration invalident sa réutilisation.
- Codex conserve des fichiers SQLite ouverts. Une VM vierge déjà initialisée ne
  doit pas recevoir le disque d'une autre conversation sous ces descripteurs.
  Une reprise utilise sa VM conservée ou le chemin froid sur son disque existant.
- Les VM prêtes et conservées comptent dans les slots et la RAM physique. Leur
  nombre et leur durée de conservation sont bornés ; une exécution active est
  prioritaire. Un slot n'est libéré qu'après arrêt du VMM et de son backend.
- Une pause ne remplace pas une sauvegarde. La fin du tour doit fermer ses relais
  et confirmer les écritures durables ; sauvegarde, suppression et migration
  restent coordonnées avec le propriétaire du disque. Un état inconnu, une
  annulation ou une erreur de stockage entraîne le teardown sûr existant.

Avant activation : mesurer le premier appel authentifié intégré, les reprises,
la saturation et l'éviction du pool ; vérifier les sauvegardes répétées, la reprise
après crash, l'isolement entre propriétaires et l'absence de redémarrage intempestif.
Les essais présents valident le levier de démarrage, pas ce cycle de vie complet.

## Première brique d'intégration : processus Codex résident

Le candidat ajoute un service invité privé qui possède un Codex initialisé et
exécute un seul tour à la fois via le même adaptateur Rust que le chemin froid.
L'initialisation ne demande pas de compte. Chaque tour se connecte au broker du
compte, puis libère cette authentification avant de confirmer sa fin. Une perte
du client, un état inconnu ou une erreur retire le processus ; une initialisation
interrompue attend également sa terminaison. Le contrôleur n'active pas encore
ce service et ne conserve pas encore les VM : cette section décrit le candidat.

La configuration MCP est fournie à chaque thread, avec le token courant du
gateway. L'environnement du processus conservé ne porte pas un token de tour.
Le service désabonne son thread après succès. Le [code de Codex 0.159.3](https://github.com/openai/codex/blob/rust-v0.159.3/codex-rs/app-server/src/request_processors/thread_processor.rs)
peut ignorer des overrides sur un thread chargé et abonné ; un thread idle sans
abonné est reconstruit lorsque la reprise apporte des overrides. Il faut donc
tester cette reprise avec le vrai binaire, en changeant les accès.

Le test reproductible `tests/fixtures/ready-codex.mjs LEO CODEX` utilise des
homes temporaires, le vrai binaire 0.159.3 extrait de l'image v0.50.6, un modèle
HTTP simulé et un MCP local. Il vérifie le même PID natif sur trois tours, le
contexte, le changement du token et des outils MCP, puis leur suppression.
L'arrêt du service doit récupérer le processus natif. Le modèle annoncé est
`gpt-5.4`, dont les capacités d'outils sont connues du catalogue local embarqué ;
aucun véritable modèle n'est interrogé.

Sur l'hôte i9-14900K / noyau 7.2.6, hors VM et sans autre compilation, trois
séries donnent les médianes suivantes avant la requête modèle : démarrage froid
de l'adaptateur 151 ms, première attribution prête 57 ms, reprise 34 ms.
Le binaire Rust est un build `dev` ; le cache de fichiers n'est pas évincé.
« Froid » signifie ici un nouveau processus, pas un disque froid.
Le préchauffage préalable prend 88 ms. Ces mesures isolent l'adaptateur ; elles
ne se comparent pas directement aux 8,75–10,05 s du chemin de production complet.
Les cinq tests ciblés supplémentaires couvrent aussi le rejet d'un tour
concurrent, la perte du client, l'arrêt pendant l'initialisation et le cycle
login/refresh/logout avec un broker synthétique. L'intégration du pool, ses
limites, l'éviction et les sauvegardes de VM conservées restent à valider avant
activation ou release.

## Attribution du disque préchauffé

Le candidat conserve les disques préchauffés dans `environments/<UUID>`.
L'attribution réserve d'abord un pointeur dans `disks/<conversation>`, puis
écrit le propriétaire permanent dans l'environnement. Une attribution
interrompue empêche ainsi de réclamer un second disque pour la même conversation.
Les fichiers et leurs répertoires sont
synchronisés avant confirmation ; aucun journal ouvert n'est déplacé. Le chemin
classique `disks/<conversation>` reste compatible pour les disques existants.

Un verrou d'appartenance protège l'attribution et la résolution pendant le
bootstrap, l'exécution et le teardown. Les mutations prennent également le
verrou physique. Au redémarrage, après clôture des anciens VMM, le contrôleur
termine une attribution interrompue et élimine uniquement les environnements
sans propriétaire. Un conflit avec un autre propriétaire ou un disque existant
arrête cette récupération sans supprimer les données. Une suppression conserve
les inodes de verrou et les records d'appartenance : le disque ne retourne jamais
dans le pool anonyme.

Les sauvegardes, restaurations, reçus, quotas et caches résolvent ce même disque.
Les tests couvrent les deux ordres de clôture des verrous, le crash entre les
deux écritures, les conflits, les alias, la suppression et les sauvegardes sur
un journal déjà ouvert. Cette étape prépare le cycle de vie du pool ; elle
n'active pas encore le préchauffage et ne représente pas un gain de démarrage.

Le disque anonyme est créé avec une base entièrement locale et sans grant S3.
Sa source refuse les lectures distantes et la publication avant attribution.
Après attribution durable, l'autorisation de la conversation est enregistrée
dans le journal et synchronisée avant d'être attachée à la source ouverte.
Les imports utilisateur ne peuvent commencer avant cette étape. La récupération
d'une attribution interrompue avant l'autorisation suit également cette règle.
Une source déjà autorisée conserve son grant : les reçus de publication et les
pins de sa base montée restent liés à cette identité.

Les tests vérifient la création réelle d'un ext4 local, le refus des bases
distantes anonymes, la persistance de l'autorisation, les écritures avant et
après attribution sur le même journal, la réouverture et l'annulation d'une
lecture distante. Cela prépare le bootstrap du pool, encore non activé.

### Cycle de vie du service dans l'invité

Le protocole invité expose maintenant un préchauffage explicite et sa capacité,
absente sur les anciennes images. Seule une VM sans conversation ni état Codex
importé peut initialiser le service. Elle installe les schémas SQLite et la
politique commune de stockage des credentials, sans compte, puis lance le vrai
adaptateur sous UID 1000 avec un environnement fixe. La disponibilité repose
sur un message borné envoyé après l'initialisation native, jamais sur la seule
présence d'un socket. Le contrôleur règle l'horloge avant ce préchauffage.

Les imports préservent les bases ouvertes et le binaire exécuté par le service.
Les logs de l'adaptateur résident rejoignent uniquement le tour actif ; leur
détachement annule aussi une émission bloquée et permet au tour de finir alors
que Codex reste vivant. Une perte du service invalide la VM préparée : aucun
second processus ne démarre silencieusement sur ses bases ouvertes. L'arrêt de
l'invité ferme le service et attend ses processus ; un adaptateur qui ne répond
plus entraîne l'arrêt de son groupe complet.

Le test natif sans fournisseur dans la configuration initiale vérifie les
overrides de modèle et de MCP par tour, leur renouvellement et leur suppression,
la conservation du contexte et la fermeture du processus natif. Trois essais
Firecracker réels avec le journal vhost, 7 vCPU et 35 840 MiB ont également
conservé Codex disponible après attribution durable du disque et supprimé le
socket du VMM après arrêt. Ces essais fonctionnels étaient concurrents aux
tests backend : leurs durées ne constituent pas un comparatif de performances.
Ils n'exercent pas encore l'admission du pool, une conversation authentifiée ou
la publication S3. Les traces brutes restent hors du dépôt ; la qualification
intégrée et ses assets accompagneront la release avec gain mesuré.

## Preuves et politique de livraison

### Admission et premier comparatif intégré local

Le contrôleur prépare au plus une VM anonyme dans ses slots et budgets existants.
Elle expire après 60 s, un changement de budget ou une pression de ressources.
Le préchauffage requiert 2 Gio de marge. Une demande active annule la préparation
et attend la fin réelle du VMM et du backend avant de reprendre son slot ; les
sondes de santé n'attendent pas cette admission. L'arrêt du contrôleur vide aussi
le pool. Une attribution partiellement persistée conserve toujours son disque.

Seuls les nouveaux chats Codex avec le home géré standard et un disque neuf
peuvent adopter ce service. Les reprises, les homes personnalisés et les autres
exécutions gardent leur chemin à froid. Une VM anonyme n'est jamais réattribuée
après usage. Un journal encore inspecté reste conservé lors de son retrait.
`LEO_READY_VM_POOL=false` permet un contrôle à froid avec la même image.

Un premier essai du chemin HTTP du runner, sur le vrai Firecracker/vhost et le
vrai Codex 0.159.3, utilise des tokens de compte synthétiques, un modèle local et
une commande réellement exécutée par Codex. Même image et cache d'image,
7 CPU / 35 840 Mio par invité ; trois conversations neuves par variante :

| Demande runner → premier appel modèle | Sans préchauffage | VM avec Codex prêt |
| --- | ---: | ---: |
| Médiane | 2,793 s | 1,673 s |
| Minimum–maximum | 2,718–2,874 s | 1,565–4,525 s |

Le gain médian observé est de 40 %. La série conserve son essai lent à 4,525 s :
l'attribution et les imports y prennent 127 ms, le reste se situe dans le
parcours natif avant le modèle. Ce contrôle local n'inclut ni le manager,
ni les MCP métier, ni une authentification réelle, ni S3/WAN. Il ne remplace pas
la référence de production de 8,75–10,05 s et ne permet pas d'annoncer un nouveau
temps en production. Les tests natifs vérifient aussi les skills ajoutés après
préchauffage, les leases de compte par tour et le contexte entre tours.

Les trois disques attribués ont ensuite été publiés vers l'origine immutable
locale : 35–36 blocs distincts par disque, tous relus et vérifiés par leur hash.
Un nouveau VMM à froid a repris chaque thread avec le même identifiant et son
contexte. Cela vérifie le chemin de publication et de reprise après attribution,
sans revendiquer un test du chiffrement S3 ou de la latence WAN.

Les échantillons et le harnais restent hors du dépôt, en attendant les assets
de la release qualifiée avec gain intégré. Aucune release supplémentaire n'est
créée pour cette étape seule.

Les [mesures vhost](https://github.com/leo91000/leo-agent-manager/releases/download/v0.50.6/ready-vm-vhost-2026-10-01.json),
les [mesures snapshot](https://github.com/leo91000/leo-agent-manager/releases/download/v0.50.6/ready-vm-snapshot-2026-10-01.json)
et le [harnais jetable](https://github.com/leo91000/leo-agent-manager/releases/download/v0.50.6/booted-vm-prototype-2026-10-01.tar.gz)
sont des assets de la release existante. Le harnais s'exécute depuis un checkout
du projet avec Node et Docker/KVM ; `ready-vm-benchmark.mjs IMAGE vhost|direct`
crée et supprime uniquement ses fixtures. Aucun dump de mémoire invitée, disque
ou identifiant d'authentification réel n'est publié.

Les huit anciens JSON de `docs/benchmarks` sont également déplacés dans les
assets de v0.50.6, octets inchangés et SHA-256 vérifiés. Leur contenu conserve
ses dates et périmètres historiques. Les liens documentaires pointent vers les
assets ; aucun historique Git n'est réécrit.

## Validation de charge et priorité avant release

Le test intégré du candidat dure 308,66 s, avec 47 publications et 24 290
écritures durables : p99 final 27,88 ms, maximum 817,29 ms, aucune erreur de
lecture/écriture ni redémarrage intempestif. Les vérifications de réouverture,
restauration différée, publication sous pression et reprise passent également.
Ce banc local ne remplace pas la qualification S3 et crash de l'image de release.

Un test dans le vrai Codex invité vérifie Cargo et les chemins Android depuis
un shell sans profil. Il échoue avant correction et passe après partage de
l'environnement des toolchains entre le lancement froid et le résident. Trois
disques attribués sont publiés puis reprennent le même thread après reboot.

Avant release, comparer les nouvelles conversations et reprises sur le parcours
de production complet, avec et sans pool. Le `turn_started` natif est une borne
de démarrage de tour, pas la preuve du premier appel réseau au modèle. L'écart
avec un modèle et un compte simulés ne doit pas être attribué au manager par
soustraction : mesurer chaque frontière, notamment préparation invitée, broker,
MCP et appel modèle. Mesurer aussi fsync, petites écritures, git status et
installation de dépendances sur un vrai dépôt contre un disque ext4 direct.

Pour les reprises, envisager une conservation bornée de la VM et du Codex de la
même conversation, sous le même propriétaire exclusif. Renouveler le compte et
les droits à chaque tour ; évincer avant admission en cas de pression ou de
configuration incompatible. Après éviction, préférer un nœud autorisé possédant
déjà le journal et les blocs, sans bloquer une migration ou perdre la reprise
depuis l'état publié. Cette stratégie n'est pas encore implémentée.

Pour ce chantier de performance, regrouper les changements : une nouvelle
release exige un gain mesuré sur le chemin intégré, avec une image qualifiée.
Un prototype, une instrumentation ou un nettoyage documentaire seul ne déclenche
pas de release. Conserver uniquement les résumés et décisions dans les ADR,
et publier les échantillons volumineux en assets.
