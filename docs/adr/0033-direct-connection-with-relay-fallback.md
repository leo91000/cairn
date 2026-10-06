# Connexion directe quand le réseau le permet, relais de secours sinon

Date : 2026-10-06. Révise la première conséquence de l’ADR-0028 (tout le trafic
de l’interface passe par le relais). Le modèle d’installations autonomes, le
compte Leo comme seul accès et les règles de sécurité restent inchangés.
Plan : #98.

L’interface (web et Android) échange les messages, l’API et les flux des
conversations avec l’installation par une **connexion directe** quand le réseau
le permet. Le relais existant du service officiel reste la **voie de secours** :
il sert dès l’ouverture, quand le direct échoue (NAT symétrique, UDP bloqué,
pare-feu) et pendant une bascule. Le choix de la route est automatique, comme
le principe direct/relais du réseau de Steam, sans en imposer le SDK.

La connexion directe utilise WebRTC (ICE avec STUN, puis un DataChannel chiffré
par DTLS). C’est la seule technique disponible dans un navigateur qui joint une
machine sans port entrant, sans domaine ni certificat public : l’installation
n’ouvre que des flux UDP sortants, et le certificat DTLS auto-signé de chaque
pair est épinglé par son empreinte, transmise par la signalisation authentifiée.
Une connexion HTTPS ou WebSocket directe vers l’installation a de nouveau été
écartée pour les raisons de l’ADR-0028 (domaine, certificat, port entrant).

## Séparation des rôles

- **Signalisation et autorisation** : toujours le service officiel. Il vérifie
  la session, le rôle et l’installation, puis délivre une autorisation de
  connexion directe signée et transmet l’offre, la réponse et les candidats ICE
  par le tunnel déjà authentifié de l’installation.
- **Transport du contenu** : le DataChannel direct, ou le relais en secours. Les
  deux transportent les mêmes trames du protocole applicatif (requêtes,
  réponses, flux à crédits, annulation) ; l’installation les traite avec le
  même dispatcher, seule la source de l’identité change.

## Autorisation d’une connexion directe

L’installation n’accepte un DataChannel qu’avec une autorisation signée par le
service officiel, vérifiée avec une clé publique reçue par son tunnel
authentifié. L’autorisation désigne l’installation, le compte Leo, le rôle, la
session, la génération d’accès, l’empreinte DTLS du client et une échéance de
quelques minutes. L’installation compare l’empreinte du pair après la poignée
de main DTLS : une autorisation volée ne sert à aucun autre client.

Le client renouvelle l’autorisation avant son échéance par le service officiel,
qui revérifie la session et le rôle. Le service officiel notifie aussitôt
l’installation, par le tunnel, de toute déconnexion, expiration ou révocation de
session, retrait de membre, changement de rôle, détachement, rotation ou
révocation définitive ; l’installation ferme les canaux concernés. Si le tunnel
est coupé, aucune nouvelle connexion directe n’est possible et celles en cours
se ferment à l’échéance de leur autorisation : le service officiel reste
l’autorité. Il n’y a ni accès local anonyme, ni mot de passe local.

## Ce qui reste sur le service officiel

- Comptes, connexions, sessions, revendications, partage, audit, disponibilité
  et version approuvée des installations ; la signalisation elle-même.
- Les clients MCP externes : ils exigent une URL HTTPS publique avec OAuth et ne
  parlent pas WebRTC.
- Les liens publics des livrables : destinataires anonymes, URL stable, en-têtes
  de sécurité imposés par le service officiel.
- Les notifications push : les clés Web Push et Android sont tenues par le
  service officiel, qui doit joindre un appareil dont l’application est fermée.
- Le relais de secours.

## Ce que voit le service officiel

- Connexion directe : uniquement les métadonnées de signalisation (compte,
  installation, horaires, empreintes DTLS et candidats ICE, donc adresses IP),
  jamais le contenu.
- Relais de secours : le contenu en mémoire, en transit, sans le conserver,
  comme aujourd’hui.
- Dans tous les cas, le service officiel sert le code de l’interface web : la
  connexion directe protège le contenu des copies et des journaux du central,
  pas d’un service officiel compromis. Ce n’est pas un chiffrement de bout en
  bout contre lui.

Le stockage ne change pas : conversations, projets, fichiers, secrets et disques
restent sur l’installation, ses nodes et leur S3. L’appareil n’est pas l’unique
copie de l’historique.

## Options écartées ou différées

- Relais TURN : il garderait le chiffrement DTLS de bout en bout en secours,
  mais demande d’exploiter des serveurs TURN. Le relais applicatif existant
  couvre déjà les réseaux bloqués sur HTTPS 443 ; TURN est différé et pourra
  s’ajouter comme route intermédiaire sans changer ce modèle.
- SDK Steam : inutilisable dans un navigateur.
- Ressources binaires (images, pièces jointes, téléchargements) : elles restent
  sur le relais dans la première livraison, car un DataChannel ne sert pas
  directement une URL de ressource. Les messages, l’API et les flux passent en
  direct.

## Conséquences

- Le protocole du relais gagne des trames de signalisation, de renouvellement et
  de révocation des connexions directes, négociées par version comme les
  précédentes ; une installation ou un client trop ancien reste sur le relais.
- Une requête en cours lors d’une bascule n’est rejouée que si elle est sans
  effet (lecture, flux repris avec son curseur) ou porte un identifiant client
  (envoi de message : l’identifiant déjà utilisé vaut succès). Les autres
  échouent visiblement, comme une coupure du relais aujourd’hui.
- L’installation doit pouvoir émettre de l’UDP sortant pour le direct ; sinon
  tout passe par le relais. Un réglage permet de désactiver le direct.
- L’ADR-0032 (un seul processus officiel) reste nécessaire pour la signalisation
  et la notification immédiate des révocations.
