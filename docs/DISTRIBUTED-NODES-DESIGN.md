> Validation globale reçue le 26 septembre 2026 : implémentation et création
> d’une PR autorisées. Les mentions d’attente de validation ci-dessous décrivent
> les étapes antérieures de l’entretien ; la spécification validée fait référence.

# Nodes d’exécution distribuées

Conception issue de l’entretien. Les choix produit ci-dessous sont confirmés ;
les réglages et garanties proposés dans la synthèse finale restent à valider
avant l’implémentation. Voir [la synthèse](DISTRIBUTED-NODES-SPEC.md).

## Objectif

Utiliser des PC et serveurs enregistrés dans Leo pour le travail des agents.
La première version conserve Firecracker et couvre les exécutions CPU ; le GPU
est différé jusqu’à un accès direct pris en charge dans les VM Firecracker.
Les nodes ne sont pas nécessairement
disponibles en permanence. Le chantier couvre les capacités et tags, l’affectation
du travail, les projets, les accès aux comptes et le déplacement des conversations.

## Décisions confirmées — premier tour

- Première cible : serveurs Linux. Le matériel GPU indiqué est une RTX 4090.
  Le système actuellement installé sur le PC n’a pas été précisé ; Windows
  et macOS ne sont pas des exigences confirmées pour cette première version.
- Les nodes appartiennent à l’utilisateur ou à des administrateurs de confiance.
  Les machines tierces non fiables sont hors de ce périmètre.
- Le déplacement d’une conversation comporte une pause, un transfert des fichiers
  et de la session, puis une reprise. La mémoire et les processus en cours ne sont
  pas transférés.
- L’IA peut choisir automatiquement parmi les nodes autorisées, dans les règles
  configurées par l’utilisateur. Les règles précises restent à définir.
- Sans node compatible disponible, échec immédiat par défaut. L’IA peut demander
  explicitement une attente d’une durée déterminée avant l’échec. Seule la demande
  de capacité échoue : la conversation continue sur sa node actuelle. Les bornes
  de cette attente restent à préciser.
- Les nodes doivent se mettre à jour automatiquement en suivant les mises à jour
  du master. Les travaux actifs sont suspendus dès que possible pour appliquer
  la mise à jour, puis les conversations reprennent après redémarrage. Le rattrapage
  au retour en ligne, la compatibilité des versions et la récupération après échec
  restent à préciser.

## Décisions et précisions confirmées — deuxième tour

- L’utilisateur souhaite transférer l’environnement complet, idéalement la VM
  Firecracker elle-même, plutôt que recréer les outils à partir des seuls projets.
  Cela reste une reprise après arrêt, sans conservation des processus ni de la RAM.
- Firecracker est conservé. L’accès GPU doit être direct dans la VM de l’agent ;
  les conteneurs GPU, la délégation externe et un autre moteur de VM ne font pas
  partie de la première version. Le GPU est différé en attendant ce support.
- À terme, plusieurs conversations distinctes doivent pouvoir utiliser le même
  GPU simultanément. Cette capacité reste à vérifier indépendamment d’un futur
  accès direct ; elle n’est pas promise par le seul support GPU de Firecracker.
- Lors d’un arrêt normal de la machine, demander jusqu’à cinq minutes pour
  préparer l’arrêt, si le système le permet. Le chat doit être transmis au master
  en continu ; si le transfert nécessaire n’est pas terminé, l’interface doit
  afficher clairement que la conversation est en pause pendant le redémarrage.
- La reprise sur une autre node depuis une sauvegarde est acceptée (Q14).
  Elle est automatique dès qu’une autre node compatible et autorisée est disponible
  (Q15), après exclusion de l’ancienne exécution et à partir d’une sauvegarde
  complète utilisable. Le flux du chat ne remplace pas une copie du disque et de
  la session. Les sauvegardes périodiques avec perte possible de travail récent
  sont acceptées ; leur fréquence reste configurable.
- Q11 confirmé : suspendre et mettre à jour dès que possible, puis reprendre la
  conversation au redémarrage. Ce choix ne suppose pas la reprise exacte d’une
  commande interrompue ; le traitement des opérations en cours doit être prévu.

## Décisions et clarification — sauvegardes et déconnexion

- Q17 : sauvegardes sur le master par défaut, avec une destination S3 configurable.
  La rétention et les limites de stockage restent à définir.
- Q18 : une node qui perd le lien avec le master suspend ses travaux après un
  délai, puis peut reprendre après reconnexion et réconciliation avec le master.
  Le délai de soixante secondes est une proposition, pas une valeur confirmée.
- Une conversation déjà réaffectée ailleurs ne doit pas reprendre simultanément
  sur son ancienne node. La reconnexion ne vaut donc pas autorisation de reprise.
- Q16 confirmée après clarification : sauvegardes périodiques avec perte possible
  du travail récent acceptées. La proposition comprend une sauvegarde après
  chaque réponse terminée, avant déplacement ou mise à jour, et périodiquement
  pendant les travaux longs. Cette première proposition de cadence est ensuite
  remplacée par les incréments fréquents en arrière-plan acceptés en Q24.
- Le master reçoit les messages et événements du chat, mais
  ce flux ne constitue pas une réplication des fichiers, des outils installés,
  des données Docker ou de la session native conservée sur disque dans la VM.
  Un message annonçant une modification n’emporte pas forcément le fichier modifié.
- La réplication synchrone de chaque écriture n’est pas demandée ; Q24 précise
  des incréments asynchrones fréquents. Une sauvegarde ne devient utilisable
  qu’après réception complète de ses dépendances et vérification de sa cohérence.

## Décisions confirmées — accès et exploitation

- Q19 : nodes autorisées configurables par agent, avec un choix « toutes les
  nodes ». Les tags ne modifient pas les droits sur les projets ou les comptes.
- Q20 : l’IA peut demander CPU, RAM et disque dans des plafonds configurés par
  node. Les ressources sont réservées avant lancement ; une modification qui
  exige un redémarrage passe par une pause visible.
- Q21 : installation assistée par une commande fournie par le master, vérification
  des prérequis, association avec un code temporaire, démarrage et mises à jour
  automatiques. Connexion sortante au master, sans port entrant à ouvrir sur la node.
- Q22 : administration et suivi complets sur le web et sur Android, avec les mêmes
  états de conversation.

## Synthèse à valider

Q24 confirme le choix incrémental en arrière-plan avec un petit retard acceptable.
Le disque actif reste local ; après la base initiale, envoyer les changements par
lots, publier des points cohérents, montrer leur âge réel et conserver les
dépendances nécessaires à leur restauration. La destination reste le master par
défaut avec S3 configurable. La recherche et les limites mesurables sont dans
`INCREMENTAL-VM-BACKUP-RESEARCH.md`. La validation globale Q23 reste à donner sur
la synthèse actualisée, sans nouvelle validation présumée par cette réponse ciblée.

Les valeurs initiales, le traitement de Main, les garanties de reprise, les
credentials, les tests et le déroulé d’implémentation sont regroupés dans
`DISTRIBUTED-NODES-SPEC.md`. Cette synthèse est proposée pour validation globale ;
elle ne constitue pas une autorisation de publier, fusionner ou déployer du code.

Q15 à Q18 sont confirmées ci-dessus. La proposition initiale de quinze minutes
est remplacée par les incréments fréquents de Q24 ; aucune latence précise n’est
encore mesurée. Soixante secondes avant suspension sur déconnexion restent une
valeur configurable soumise à la validation globale.

Q19 à Q22 sont confirmées ci-dessus.

## Contraintes observées dans le dépôt

Le runner actuel utilise Firecracker. Le manager et le runner partagent un volume,
des chemins et des relais d’authentification locaux ; l’affectation est configurée
par une seule adresse de runner. La reprise et l’export de disques existent, mais
ne constituent pas encore une migration distribuée.

L’installation actuelle est Docker Compose sur Linux x86-64/KVM, sans inscription
de node distante ni identité révocable par node. Chaque VM reçoit actuellement
2 vCPU, 4 Gio de RAM et un disque sparse de 32 Gio ; la concurrence est configurable.
L’ajout de ressources par exécution modifierait donc le contrat actuel du runner.
Main ne peut pas restreindre ses projets et connexions ordinaires, alors que
1Password exige un accès explicite même pour Main. Si Q19 retient des restrictions
de nodes par agent, le traitement de Main devra être rendu explicite et cohérent
entre le backend, le web et Android.

L’export actuel refuse une VM active et produit une archive complète du disque
(`backend/src/runner.rs`, opérations `disks/export`). Il ne fournit ni sauvegarde
en activité ni transfert incrémental. Le mécanisme d’archivage S3 chiffre et
vérifie les archives, mais concerne des conversations inactives
(`backend/src/conversation_archive.rs`) ; il ne fournit pas encore de sauvegardes
périodiques de reprise. La sauvegarde doit coordonner le disque et la session.
L’image de base compatible doit rester disponible : le démarrage du runner
supprime actuellement les anciens caches d’images (`backend/src/microvm/host.rs`).

Le renouvellement des comptes Codex et Claude est centralisé ; l’intégration
1Password exécute les lectures côté serveur. Ces mécanismes constituent une base
à adapter au transport distant, et non une preuve de fonctionnement multi-node.

La distribution actuelle utilise une image GHCR validée et promue par digest.
Le commit applicatif et `APP_RUNTIME_ID` sont distincts : une mise à jour des CLI
peut changer le runtime sans changer le commit. Le runner publie son `runtimeId`,
mais aucune négociation générale de compatibilité master/node n’a été observée.
Les disques persistants sont réutilisés avec la nouvelle image : un retour à
l’image précédente ne prouve donc pas une restauration complète de l’environnement.
Le suivi automatique devra préciser ces deux identités et la compatibilité de
l’état conservé. Voir `.github/workflows/ci.yaml`, `scripts/cli-updates.mjs`
et `deploy/microvm/init` à la racine du dépôt.

Sources locales : `MICROVMS.md`, `ONEPASSWORD.md`, `CLAUDE-CODE.md`,
`../backend/src/runner.rs`, `../backend/src/project_workspaces.rs`,
`../backend/src/claude_tokens.rs` et `../compose.yaml`.

## Vérifications externes — GPU et arrêt Linux

- Le dépôt utilise Firecracker 1.17.0. Son
  [interface publiée](https://github.com/firecracker-microvm/firecracker/blob/v1.17.0/src/firecracker/swagger/firecracker.yaml)
  ne propose pas de périphérique GPU/VFIO ; le
  [chantier PCI publié](https://github.com/firecracker-microvm/firecracker/issues/5133)
  exclut VFIO. Un
  [prototype GPU existe](https://github.com/firecracker-microvm/firecracker/discussions/4845),
  mais il ne constitue pas un support de production validé.
- Plusieurs processus peuvent partager un GPU dans un même environnement Linux,
  ce qui ne signifie pas qu’il soit partageable directement entre plusieurs VM.
  La RTX 4090 ne figure pas dans les matrices officielles
  [MIG](https://docs.nvidia.com/datacenter/tesla/mig-user-guide/supported-gpus.html)
  et [vGPU](https://docs.nvidia.com/vgpu/gpus-supported-by-vgpu.html).
  Aucun quota matériel strict de VRAM par travail n’est donc promis.
- Conserver les conversations Firecracker et déléguer leurs calculs à un
  environnement GPU a été écarté : cela ne rendrait pas le GPU localement visible
  à un programme lancé dans la microVM, ce qui est le besoin exprimé.
- Le déplacement à froid d’une VM doit emporter ses disques et retrouver une
  configuration et une image de base compatibles. Les
  [snapshots Firecracker](https://github.com/firecracker-microvm/firecracker/blob/main/docs/snapshotting/snapshot-support.md)
  avec mémoire sont un mécanisme différent ; ils ne dispensent pas de gérer
  les disques persistants. La conservation de la RAM reste hors du besoin accepté.
- Un arrêt Linux normal peut être retardé par un
  [inhibiteur logind](https://github.com/systemd/systemd/blob/main/docs/INHIBITOR_LOCKS.md).
  `InhibitDelayMaxSec` a un défaut de cinq secondes ; une fenêtre de cinq minutes
  nécessite une configuration de l’hôte. `TimeoutStopSec` seul n’assure pas une
  borne globale de cinq minutes : prévoir une échéance commune aux phases d’arrêt.
  Un arrêt forcé ou une coupure électrique peut empêcher toute préparation.
- L’interface doit distinguer une pause annoncée d’une perte de connexion dont
  l’état d’exécution reste inconnu. Le chat reçu ne prouve pas que le disque a été
  transféré. La reprise doit se réconcilier avec le master avant de relancer le
  travail, pour éviter deux exécutions de la même conversation.

Ces vérifications documentaires ne constituent pas des tests sur la RTX 4090
ni sur la distribution Linux de l’utilisateur. Q12 à Q14 sont résolues : GPU
différé, objectif futur de partage entre conversations, et reprise depuis une
sauvegarde sur une autre node dans le périmètre de la première version.
