# VM pré-démarrées avec Codex prêt

Date : 2026-10-01.

Statut : prototypes mesurés ; pool retenu comme prochain candidat à intégrer.
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

## Preuves et politique de livraison

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

Pour ce chantier de performance, regrouper les changements : une nouvelle
release exige un gain mesuré sur le chemin intégré, avec une image qualifiée.
Un prototype, une instrumentation ou un nettoyage documentaire seul ne déclenche
pas de release. Conserver uniquement les résumés et décisions dans les ADR,
et publier les échantillons volumineux en assets.
