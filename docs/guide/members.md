# Member guide

This guide is for pilots who use a Tether instance: logging in, adding your characters, giving apps what they need, joining groups and linking Discord. Your admins decide what you can see and do, so some pages here may not be in your sidebar.

## Logging in

Press **Log in with EVE Online** on the sign-in page. You log in on CCP's site, and Tether never sees your password. There's no email or password of any kind.

The first character you log in with becomes your **main**. Your main decides your state (Member, Blue, Guest or one your admins made), and with it most of what you may do.

Only your main signs in. If you log in with an alt, Tether refuses with "Unable to authenticate as the selected character. Please log in with the main character associated with this account." Log in with your main instead. A session lasts 14 days.

**Log out** is at the bottom of the account menu: press your name at the top right.

## Your characters

The **Dashboard** lists the characters on your account, with your state and groups under its title.

- **Add character** logs in with another character on EVE's site and adds it to your account. Pick the character you want on EVE's login page.
- **Make main** makes one of your characters your main. Your state follows it.
- **Change Main with EVE login** logs in with any character and makes it your main, adding it to your account if it isn't on it yet.
- **Remove**, in the Characters table, takes an alt off your account. Its tokens go, and it leaves every app it was registered for. Log in with it again to add it back. Your main can't be removed: make another character your main first.

Where an admin installed Member Audit, the Dashboard is Member Audit's **My characters**: a card for each character, and **Register another character** to add one.

If a character is sold, or its EVE access ends, it leaves your account. If that was your main, your account has no main: you're Guest until you choose one, and a banner on every page offers **Change Main**.

### Acting as another character

If you have several characters, the account menu lists them under **Act as**. Pick one, and apps use it where they act for you: whose fleet attendance to record, whose fits to check, and so on. Your permissions and state stay your account's. The account menu says **Acting as** while you've picked one.

## Registering characters

Some things need more than a login. Your state may require every character on your account, main and alts, to grant Tether some EVE access. Apps that read your characters (such as Member Audit) read only the characters you register for them.

**Register Character** (from the banner, or `/register`) shows:

- **What registering grants**: the EVE access needed, in plain words.
- **Your characters**: each one's status, with **Register <name>** for each that still needs something. Each button takes you to EVE's login page; pick the character named in that row there.
- **Apps**: the apps you may use that read characters, how many of your characters are registered for each, and **Register for <app>** (or **Review**).

If your state needs something you haven't granted, you're flagged as not compliant. You keep your state, but a banner asks you to register, and some features (and anything your admins tie to compliance) wait until you do.

For one app, **Register for <app>** lists your characters with **Register <name>** and **Unregister**. Unregister a character and that app stops reading it. Apps with a character list also offer **Register another character** on their own pages.

Tether keeps every token encrypted and never shows it to anyone, apps included.

## Token Management

**Token Management**, in the account menu, shows the EVE access Tether holds for each of your characters, and what it may read with it. Open a character's scopes to see which apps use each one.

- **Refresh** checks a token with EVE now.
- **Delete** deletes Tether's copy. The character leaves your account a day later unless you log in with it again. To end the access at EVE too, revoke Tether on CCP's third-party applications page.

**App data sources** lists your characters that apps read corporation data through (added with **Add data source**, such as a Director's character for corporation structures). **Withdraw** one and the app stops reading through it.

**Access tokens**, also in the account menu, are for bots and scripts that use Tether's API as you. A token does only what you tick when you make it, and only until it expires. It never works on pages. Copy a new token at once: it isn't shown again.

## Groups

Groups add access on top of your state. **Groups** in the sidebar shows:

- **Your groups**, with **Leave**. Some groups need a leader's approval to leave; they show **Leave requested** until then. Internal groups are managed by admins, and you can't leave them yourself.
- **Available groups** you may join. **Join** takes you into an Open group at once. **Request** asks the group's leaders; **Withdraw** takes the request back.
- **Your permissions**: what your state, groups and own grants give you.

Some groups are Hidden: they aren't listed, and you join them through a direct link a leader shares.

Some groups list **Requires**: these are Secure Groups, which Tether keeps by rules (your state, corporation, skills, and so on). **Secure Groups** in the sidebar shows how you stand against each requirement now. You can join or ask once you meet them all. If a requirement stops passing, you leave the group, after its grace period if it has one. Most groups tell you when that happens.

You're told in your notifications when a request is accepted or rejected.

### Leading a group

If you lead a group, **Group Management** is in your sidebar. **Group Requests** lists join and leave requests, with **Accept** and **Reject**. **Group Membership** lists your groups, their members (with **Remove**), each group's direct join link to share, and its Audit Log.

## Discord

**Services** in the sidebar shows the services you can use. To join the alliance's Discord server, press **Link Discord** and approve on Discord's site. Tether adds you to the server with the roles for your state and groups, and keeps them in step as they change. Your server nickname may be set from your main, if your admins turned that on.

**Unlink** removes the link, and you leave the server.

If your state or groups stop including Discord access, Tether removes you from the server and unlinks you, and you're told. If you leave the server yourself, you're unlinked too; link again to come back.

If the page says "Your access doesn't include Discord", your state and groups don't give you Discord access. Ask an admin.

## What's new and notifications

After Tether is updated, a **What's new** popup opens once with the changes that concern you: the notes for everyone, and for the apps you use. **Got it** closes it. Every update's notes stay under **What's new** in the account menu.

The bell at the top shows how many notifications you haven't read, and updates live. It opens **Notifications**, your latest notifications, newest first. Opening one marks it read. **Mark all read** and **Delete all read** tidy the list, and the trash icon on a row deletes one. Tether keeps a set number per pilot (your admins choose it): when a new one arrives, the oldest goes.

You get notifications when your state changes, when a group decides on your request, when your registration or compliance changes, when you lose Discord access, and from apps (an app's name is on its notices).
